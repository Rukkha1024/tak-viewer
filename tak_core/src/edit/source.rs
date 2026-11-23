//! Selective original-stream access shared by edit preparation, creation and conflict checks.
//! Layout 2 edits use the verified logical archive streams.
use super::EditError;
use crate::{tak, takm};
use tak::container::{Container, u16_at, u32_at, END};
use tak::fast_read::{StreamMap, read_exact_at};
use std::{fs::File, path::Path};
use sha2::{Digest, Sha256};

fn error(e: impl std::fmt::Display) -> EditError { EditError::Other(e.to_string()) }
#[derive(Clone)]
pub(super) struct Stream {
    pub name: String,
    pub index: usize,
    pub map: StreamMap,
    pub bytes: Vec<u8>,
    pub deferred: Vec<(u64,u64)>,
}
pub(super) struct Source {
    pub streams: Vec<Stream>,
    pub table: Option<serde_json::Value>,
    pub header: Vec<u8>,
    pub table_bytes: Vec<u8>,
    pub seal: Option<[u8;32]>,
    pub tail_start: u64,
    pub v2_tail: Option<Vec<u8>>,
    pub len: u64,
}
fn chain(fat: &[u32], start: u32) -> Vec<u32> {
    let mut out = Vec::new(); let mut seen = std::collections::HashSet::new(); let mut cur = start;
    while cur != END && (cur as usize) < fat.len() && seen.insert(cur) { out.push(cur); cur = fat[cur as usize]; }
    out
}
fn fill(file: &File, img: &mut [u8], unit: u64, sectors: &[u32]) -> Result<(),EditError> {
    for &sector in sectors {
        let off = (sector as u64 + 1) * unit;
        let end = off.checked_add(unit).ok_or(EditError::UnsupportedLayout)?.min(img.len() as u64);
        let buf = img.get_mut(off as usize..end as usize).ok_or(EditError::UnsupportedLayout)?;
        read_exact_at(file, buf, off).map_err(error)?;
    }
    Ok(())
}
impl Source {
    pub fn read(path: &Path) -> Result<Self,EditError> {
        let file = File::open(path).map_err(error)?;
        let metadata = file.metadata().map_err(error)?;
        let stamp = (metadata.len(), metadata.modified().ok());
        let len = metadata.len();
        let mut streams = Vec::new();
        let mut magic = [0;8]; read_exact_at(&file,&mut magic,0).map_err(error)?;
        if takm::v2::is_archive(&file, len).map_err(error)? {
            let (fixed, tail, identity) = takm::restore_for_write(&file, len).map_err(error)?;
            let logical = takm::v2_write::Logical::parse(&fixed).map_err(error)?;
            let mut offset = takm::HEADER_LEN;
            for row in &logical.index.streams {
                if row.offset != offset { return Err(EditError::UnsupportedLayout); }
                offset += row.size;
                streams.push(Stream {
                    name: row.name.clone(),
                    index: row.index,
                    map: StreamMap { sector: 0, chain: vec![], len: row.size, flat: Some(row.offset) },
                    bytes: logical.stream(row).to_vec(),
                    deferred: vec![],
                });
            }
            if offset != logical.table_at as u64 { return Err(EditError::UnsupportedLayout); }
            let after = file.metadata().map_err(error)?;
            if stamp != (after.len(), after.modified().ok()) { return Err(EditError::FileChangedOnDisk); }
            return Ok(Self {
                streams,
                table: Some(logical.table),
                header: fixed[..takm::HEADER_LEN as usize].to_vec(),
                table_bytes: fixed[logical.table_at..logical.table_end].to_vec(),
                seal: Some(fixed[logical.table_end..].try_into().map_err(error)?),
                tail_start: takm::v2_write::comment_header(identity),
                v2_tail: Some(tail),
                len,
            });
        }
        let (table, header, table_bytes, seal, tail_start) = if magic == takm::MAGIC {
            let idx = takm::read_index(&file,len).map_err(error)?;
            let mut head = vec![0; takm::HEADER_LEN as usize];
            read_exact_at(&file,&mut head,0).map_err(error)?;
            let table_at = u64::from_le_bytes(head[16..24].try_into().unwrap());
            let mut table_bytes = vec![0; (idx.fixed_len-table_at) as usize];
            read_exact_at(&file,&mut table_bytes,table_at).map_err(error)?;
            let table = serde_json::from_slice(&table_bytes).map_err(error)?;
            let mut seal = [0;32]; read_exact_at(&file,&mut seal,idx.fixed_len).map_err(error)?;
            // Overlap/gaps make selective source verification ambiguous. Refuse editing.
            let mut offset = takm::HEADER_LEN;
            for s in &idx.streams {
                if s.offset != offset { return Err(EditError::UnsupportedLayout); }
                offset = offset.checked_add(s.size).ok_or(EditError::UnsupportedLayout)?;
                streams.push(Stream {name:s.name.clone(),index:s.index,map:StreamMap {sector:0,chain:vec![],len:s.size,flat:Some(s.offset)},bytes:vec![],deferred:vec![]});
            }
            if offset != table_at { return Err(EditError::UnsupportedLayout); }
            (Some(table),head,table_bytes,Some(seal),idx.tail_start())
        } else {
            if len < 512 { return Err(EditError::UnsupportedLayout); }
            let mut img=vec![0;usize::try_from(len).map_err(error)?];
            read_exact_at(&file,&mut img[..512],0).map_err(error)?;
            if u32_at(&img,72).map_err(error)? != 0 { return Err(EditError::UnsupportedLayout); }
            let unit = match u16_at(&img,30).map_err(error)? {9=>512,12=>4096,15=>32768,_=>return Err(EditError::UnsupportedLayout)};
            let count=u32_at(&img,44).map_err(error)? as usize;
            if count>109 {return Err(EditError::UnsupportedLayout);}
            let sectors=(0..count).map(|i|u32_at(&img,76+4*i).map_err(error)).collect::<Result<Vec<_>,_>>()?;
            fill(&file,&mut img,unit,&sectors)?;
            let mut fat=Vec::new();
            for sector in sectors {
                let off=(sector as u64+1)*unit;
                let bytes=img.get(off as usize..(off+unit) as usize).ok_or(EditError::UnsupportedLayout)?;
                fat.extend(bytes.chunks_exact(4).map(|b|u32::from_le_bytes(b.try_into().unwrap())));
            }
            let dir=chain(&fat,u32_at(&img,48).map_err(error)?);
            fill(&file,&mut img,unit,&dir)?;
            let mini=chain(&fat,u32_at(&img,60).map_err(error)?);
            fill(&file,&mut img,unit,&mini)?;
            let c=Container::parse(&img).map_err(error)?;
            for (index,(name,entry)) in c.streams().map_err(error)?.into_iter().enumerate() {
                let (sector,offsets)=c.physical_sectors(entry).map_err(error)?;
                let chain=offsets.into_iter().map(|off|u32::try_from(off/sector-1).map_err(error)).collect::<Result<Vec<_>,_>>()?;
                streams.push(Stream {name,index,map:StreamMap {sector,chain,len:c.entries[entry].size,flat:None},bytes:vec![],deferred:vec![]});
            }
            (None,vec![],vec![],None,len)
        };
        let unique = |name: &str| -> Result<usize,EditError> {
            let mut hits=streams.iter().enumerate().filter(|(_,s)|s.name==name);
            let n=hits.next().ok_or(EditError::UnsupportedLayout)?.0;
            if hits.next().is_some(){return Err(EditError::UnsupportedLayout);} Ok(n)
        };
        let ni=unique("Nodes.dat")?; let ci=unique("Channels.dat")?; unique("MetaData.dat")?;
        let nodes=streams[ni].map.read(&file,0,streams[ni].map.len).map_err(error)?;
        let (channels,deferred)=tak::fast_read::edit_channels(&file,&streams[ci].map,&nodes).map_err(error)?;
        streams[ni].bytes=nodes; streams[ci].bytes=channels; streams[ci].deferred=deferred;
        for (i,s) in streams.iter_mut().enumerate() {
            if i!=ni && i!=ci && s.name!=takm::CAMERA_STREAM {s.bytes=s.map.read(&file,0,s.map.len).map_err(error)?;}
        }
        let after=file.metadata().map_err(error)?;
        if stamp!=(after.len(),after.modified().ok()){return Err(EditError::FileChangedOnDisk);}
        Ok(Self {streams,table,header,table_bytes,seal,tail_start,v2_tail:None,len})
    }
    pub fn unique(&self,name:&str)->Result<&Stream,EditError>{
        let mut hits=self.streams.iter().filter(|s|s.name==name);
        let s=hits.next().ok_or(EditError::UnsupportedLayout)?;
        if hits.next().is_some(){return Err(EditError::UnsupportedLayout);} Ok(s)
    }
    pub fn digest(&self,path:&Path)->Result<[u8;32],EditError>{
        let mut h=Sha256::new();
        h.update(&self.header);h.update(&self.table_bytes);
        if let Some(seal)=self.seal {h.update(seal);}
        for s in &self.streams {
            h.update((s.index as u64).to_le_bytes());h.update((s.name.len() as u64).to_le_bytes());h.update(s.name.as_bytes());
            h.update(s.map.len.to_le_bytes());
            for (off,n) in s.map.pieces(0,s.map.len) {h.update(off.to_le_bytes());h.update((n as u64).to_le_bytes());}
            let mut prev=0;
            for &(a,b) in &s.deferred {
                h.update(a.to_le_bytes());h.update(b.to_le_bytes());
                h.update(&s.bytes[prev..a as usize]);prev=b as usize;
            }
            h.update(&s.bytes[prev..]); // hash actual reads only, excluding deferred placeholders.
        }
        if self.seal.is_none() {
            let m=std::fs::metadata(path).map_err(error)?;h.update(m.len().to_le_bytes());
            if let Some(t)=m.modified().ok().and_then(|t|t.duration_since(std::time::UNIX_EPOCH).ok()){h.update(t.as_nanos().to_le_bytes());}
        }
        Ok(h.finalize().into())
    }
}
