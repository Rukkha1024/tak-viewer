//! Save passthrough: deferred samples travel source -> bounded buffer -> output only.
use super::*;
use crate::edit::source::{Source,Stream};

pub(super) struct Data {
    source: Source,
    file: File,
    streams: Vec<Stream>,
    table: Value,
    tail: Vec<u8>,
    output_table: Option<Value>,
    output_seal: Option<[u8;32]>,
}
impl Data {
    pub fn read(path:&Path,snapshot:&DocumentSnapshot)->Result<Self,EditError>{
        let source=Source::read(path)?;
        let file=File::open(path).map_err(error)?;
        let mut streams=source.streams.iter().filter(|s|s.name!=takm::CAMERA_STREAM || source.seal.is_some()).cloned().collect::<Vec<_>>();
        let ni=streams.iter().position(|s|s.name=="Nodes.dat").ok_or(EditError::UnsupportedLayout)?;
        let ci=streams.iter().position(|s|s.name=="Channels.dat").ok_or(EditError::UnsupportedLayout)?;
        let (nodes,channels)=serialize_markers(snapshot,&streams[ni].bytes,&streams[ci].bytes)?;
        // Analog records stay in order. Their new positions may move after marker edits.
        let old=tak::channels::iter_channel_inventory(&streams[ci].bytes).map_err(error)?;
        let new=tak::channels::iter_channel_inventory(&channels).map_err(error)?;
        let mut moved=Vec::new();
        for &(a,b) in &streams[ci].deferred {
            let rec=old.iter().find(|r|r.data_start as u64==a && r.data_end as u64==b).ok_or(EditError::UnsupportedLayout)?;
            let occurrence=old.iter().take_while(|r|r.start!=rec.start).filter(|r|r.uid==rec.uid && r.name==rec.name && r.kind==rec.kind).count();
            let other=new.iter().filter(|r|r.uid==rec.uid && r.name==rec.name && r.kind==rec.kind).nth(occurrence).ok_or(EditError::UnsupportedLayout)?;
            if other.data_end-other.data_start != (b-a) as usize {return Err(EditError::UnsupportedLayout);}
            moved.push((other.data_start as u64,other.data_end as u64));
        }
        streams[ni].bytes=nodes;streams[ci].bytes=channels;streams[ci].deferred=moved;
        let mut tail=Vec::new();
        let table=if let Some(table)=&source.table {
            if let Some(raw) = &source.v2_tail {
                tail.clone_from(raw);
            } else {
                let mut f=file.try_clone().map_err(error)?;
                f.seek(SeekFrom::Start(source.tail_start)).map_err(error)?;f.read_to_end(&mut tail).map_err(error)?;
            }
            table.clone()
        }else{
            tail=comments::header_line().into_bytes();
            if let Some(parsed)=comments::read(&comments::path_for(path)).map_err(error)?{for event in parsed.events{tail.extend_from_slice(comments::encode(&event).as_bytes());}}
            let mut dropped=Vec::new();
            for s in source.streams.iter().filter(|s|s.name==takm::CAMERA_STREAM){
                let mut h=Sha256::new();
                for (off,n) in s.map.pieces(0,s.map.len){let mut b=vec![0;n];tak::fast_read::read_exact_at(&file,&mut b,off).map_err(error)?;h.update(b);}
                dropped.push(json!({"name":s.name,"index":s.index,"size":s.map.len,"sha256":format!("{:x}",h.finalize())}));
            }
            let secs=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0,|d|d.as_secs() as i64);
            // The complete source hash is filled while staging deferred bytes during save.
            json!({"format":"takm","layout":takm::LAYOUT,"made_by":format!("tak_viewer {}",env!("CARGO_PKG_VERSION")),
                "made_at":comments::iso_time(comments::civil_from_unix(secs),0),
                "source":{"name":path.file_name().unwrap_or_default().to_string_lossy(),"size":source.len,"sha256":"0".repeat(64)},
                "streams":[],"dropped":dropped})
        };
        Ok(Self{source,file,streams,table,tail,output_table:None,output_seal:None})
    }
    pub fn write(
        &mut self, file: &mut File, save_v2: bool,
        inject: &mut impl FnMut(&str) -> Result<(), EditError>,
    ) -> Result<u64, EditError> {
        let fixed = self.write_logical(file, save_v2, inject)?;
        if !save_v2 {
            return Ok(fixed);
        }
        let mut logical = vec![0; (fixed + takm::SEAL_LEN) as usize];
        tak::fast_read::read_exact_at(file, &mut logical, 0).map_err(error)?;
        file.seek(SeekFrom::Start(0)).map_err(error)?;
        let written = takm::v2_write::archive(file, &logical, &self.tail).map_err(error)?;
        file.set_len(written.size).map_err(error)?;
        file.sync_all().map_err(error)?;
        takm::validate_v2_write(file, written.size, &logical, &self.tail).map_err(error)?;
        Ok(written.fixed_identity)
    }

    fn write_logical(
        &mut self, file: &mut File, save_v2: bool,
        inject: &mut impl FnMut(&str) -> Result<(), EditError>,
    ) -> Result<u64, EditError> {
        let mut offset=takm::HEADER_LEN;
        let mut entries=Vec::new();
        for (i, s) in self.streams.iter().enumerate() {
            let ordinal = self.source.table.as_ref().map_or_else(
                || json!(s.index), |table| table["streams"][i]["index"].clone(),
            );
            entries.push(json!({"name":s.name,"index":ordinal,"offset":offset,"size":s.bytes.len(),"sha256":"0".repeat(64)}));
            offset=offset.checked_add(s.bytes.len() as u64).ok_or(EditError::UnsupportedLayout)?;
        }
        // Stage deferred samples directly into their final output positions while hashing
        // the source once in physical file order. This preserves the complete TAK provenance
        // hash even for fragmented sector chains, with bounded memory and one source sample
        // read. Final sequential emission reuses these staged bytes for hashes/sealing.
        let mut copies=Vec::new();
        for (i,stream) in self.streams.iter().enumerate() {
            let base=self.source.streams.iter().find(|o|o.index==stream.index && o.name==stream.name).ok_or(EditError::UnsupportedLayout)?;
            let output_base=entries[i]["offset"].as_u64().ok_or(EditError::UnsupportedLayout)?;
            for (&(a,b),&(new_a,new_b)) in base.deferred.iter().zip(&stream.deferred) {
                if b-a!=new_b-new_a{return Err(EditError::UnsupportedLayout);}
                let mut output_at=output_base+new_a;
                for (input_at,len) in base.map.pieces(a,b){copies.push((input_at,input_at+len as u64,output_at));output_at+=len as u64;}
            }
        }
        copies.sort_unstable();
        let before=self.file.metadata().map_err(error)?;
        let mut hash=Sha256::new();let mut buffer=vec![0;64*1024];let mut off=0;let mut first=0;
        while (self.source.table.is_none() || !copies.is_empty()) && off<self.source.len {
            let n=(self.source.len-off).min(buffer.len() as u64) as usize;let chunk=&mut buffer[..n];
            tak::fast_read::read_exact_at(&self.file,chunk,off).map_err(error)?;hash.update(&*chunk);
            while first<copies.len() && copies[first].1<=off {first+=1;}
            for &(a,b,dest) in copies[first..].iter().take_while(|c|c.0<off+n as u64){
                let lo=a.max(off);let hi=b.min(off+n as u64);
                if lo<hi {file.seek(SeekFrom::Start(dest+lo-a)).map_err(error)?;file.write_all(&chunk[(lo-off) as usize..(hi-off) as usize]).map_err(error)?;}
            }
            off+=n as u64;
        }
        let after=self.file.metadata().map_err(error)?;
        if (before.len(),before.modified().ok())!=(after.len(),after.modified().ok()){return Err(conflict());}
        if self.source.table.is_none(){self.table["source"]["sha256"]=format!("{:x}",hash.finalize()).into();}
        file.seek(SeekFrom::Start(0)).map_err(error)?;
        self.table["streams"]=entries.into();
        let template=serde_json::to_vec(&self.table).map_err(error)?;
        let fixed=offset.checked_add(template.len() as u64).ok_or(EditError::UnsupportedLayout)?;
        if save_v2 && fixed > takm::v2_write::MAX_LOGICAL as u64 - takm::SEAL_LEN {
            return Err(EditError::UnsupportedLayout);
        }
        let mut header=[0;takm::HEADER_LEN as usize];
        header[..8].copy_from_slice(&takm::MAGIC);header[8..12].copy_from_slice(&takm::LAYOUT.to_le_bytes());
        header[16..24].copy_from_slice(&offset.to_le_bytes());header[24..32].copy_from_slice(&(template.len() as u64).to_le_bytes());header[32..40].copy_from_slice(&fixed.to_le_bytes());
        let mut output=Sha256::new();file.write_all(&header).map_err(error)?;output.update(header);inject("during_write")?;
        let mut original=Sha256::new();original.update(&self.source.header);
        let mut buffer=vec![0;64*1024];
        for (i,s) in self.streams.iter().enumerate(){
            let base=self.source.streams.iter().find(|o|o.index==s.index && o.name==s.name).ok_or(EditError::UnsupportedLayout)?;
            let mut old=Sha256::new();let mut new=Sha256::new();let mut a=0usize;let mut prev=0usize;
            if base.deferred.len()!=s.deferred.len(){return Err(EditError::UnsupportedLayout);}
            for (&(start,end),&(new_start,new_end)) in base.deferred.iter().zip(&s.deferred){
                if new_end-new_start!=end-start{return Err(EditError::UnsupportedLayout);}
                let before=&base.bytes[prev..start as usize];original.update(before);old.update(before);
                let before=&s.bytes[a..new_start as usize];file.write_all(before).map_err(error)?;output.update(before);new.update(before);
                let stage_at=entries_offset(&self.table,i)?+new_start;
                let mut done=0u64;
                while done<end-start {
                    let size=(end-start-done).min(buffer.len() as u64) as usize;let b=&mut buffer[..size];
                    tak::fast_read::read_exact_at(file,b,stage_at+done).map_err(error)?;
                    file.seek(SeekFrom::Start(stage_at+done)).map_err(error)?;
                    file.write_all(b).map_err(error)?;output.update(&*b);new.update(&*b);original.update(&*b);old.update(&*b);done+=size as u64;
                }
                prev=end as usize;a=new_end as usize;
            }
            let bytes=&base.bytes[prev..];original.update(bytes);old.update(bytes);
            let bytes=&s.bytes[a..];file.write_all(bytes).map_err(error)?;output.update(bytes);new.update(bytes);
            let old_digest=format!("{:x}",old.finalize());
            if let Some(expected)=self.source.table.as_ref().and_then(|t|t["streams"][i]["sha256"].as_str()){
                if expected!=old_digest{return Err(conflict());}
            }
            self.table["streams"][i]["sha256"]=format!("{:x}",new.finalize()).into();
        }
        if let Some(seal)=self.source.seal{original.update(&self.source.table_bytes);if <[u8;32]>::from(original.finalize())!=seal{return Err(conflict());}}
        let table=serde_json::to_vec(&self.table).map_err(error)?;
        if table.len()!=template.len(){return Err(EditError::UnsupportedLayout);}
        file.write_all(&table).map_err(error)?;output.update(&table);
        let seal:[u8;32]=output.finalize().into();file.write_all(&seal).map_err(error)?;file.write_all(&self.tail).map_err(error)?;
        file.flush().map_err(error)?;file.sync_all().map_err(error)?;
        self.output_table=Some(self.table.clone());self.output_seal=Some(seal);Ok(fixed)
    }
    pub fn validate(&self,path:&Path,snapshot:&DocumentSnapshot)->Result<(),EditError>{
        let actual=Source::read(path)?;
        if actual.table.as_ref()!=self.output_table.as_ref() || actual.seal!=self.output_seal || actual.streams.len()!=self.streams.len(){return Err(EditError::UnsupportedLayout);}
        for (a,b) in actual.streams.iter().zip(&self.streams){
            if a.index != b.index || a.name != b.name || a.bytes.len() != b.bytes.len() {
                return Err(EditError::UnsupportedLayout);
            }
            if actual.v2_tail.is_some() {
                let mut cursor = 0;
                for &(start, end) in &b.deferred {
                    if a.bytes[cursor..start as usize] != b.bytes[cursor..start as usize] {
                        return Err(EditError::UnsupportedLayout);
                    }
                    cursor = end as usize;
                }
                if a.bytes[cursor..] != b.bytes[cursor..] {
                    return Err(EditError::UnsupportedLayout);
                }
            } else if a.bytes != b.bytes || a.deferred != b.deferred {
                return Err(EditError::UnsupportedLayout);
            }
        }
        let tail = if let Some(tail) = actual.v2_tail {
            tail
        } else {
            let mut file=File::open(path).map_err(error)?;file.seek(SeekFrom::Start(actual.tail_start)).map_err(error)?;
            let mut tail=Vec::new();file.read_to_end(&mut tail).map_err(error)?;
            tail
        };
        if tail!=self.tail{return Err(EditError::UnsupportedLayout);}
        let (prepared,_)=PreparedInput::read_from_path(path)?;
        let mut doc=crate::edit::EditDocument::unloaded(path.to_path_buf());doc.install_prepared(doc.revision(),prepared)?;
        let reopened=doc.snapshot();
        if reopened.tracks().count()!=snapshot.tracks().count(){return Err(EditError::UnsupportedLayout);}
        for t in snapshot.tracks(){let other=reopened.track(&t.uid).ok_or(EditError::UnsupportedLayout)?;
            if t.name!=other.name || t.label!=other.label || t.samples!=other.samples || t.records.node!=other.records.node{return Err(EditError::UnsupportedLayout);}}
        Ok(())
    }
}

fn entries_offset(table:&Value,index:usize)->Result<u64,EditError>{table["streams"][index]["offset"].as_u64().ok_or(EditError::UnsupportedLayout)}
