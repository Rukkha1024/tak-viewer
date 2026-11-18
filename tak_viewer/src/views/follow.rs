use super::{v3, Vec3, View};

impl View {
    pub(crate) fn camera_ready(&self) -> bool {
        self.data.is_some() && self.loading.is_none() && self.remote_loading.is_none()
            && self.waiting_shared.is_none()
    }

    pub(crate) fn toggle_follow(&mut self) {
        if !self.camera_ready() {
            return;
        }
        if self.cam.follow {
            self.cam.pivot = self.follow_target.unwrap_or(self.cam.pivot);
        } else {
            self.follow_target = None;
        }
        self.cam.follow = !self.cam.follow;
    }

    /// Keep automatic motion out of the manual camera state observed by undo.
    pub(crate) fn update_follow_target(&mut self) -> Option<Vec3> {
        if !self.cam.follow {
            return None;
        }
        if let Some(data) = self.data.as_deref() {
            let points = self.selected.iter()
                .filter_map(|&index| data.markers.get(index)?.position(self.pb.frame))
                .filter(|point| point.is_finite());
            let (mut sum, mut count) = ([0.0f64; 3], 0usize);
            for point in points {
                sum[0] += f64::from(point.x);
                sum[1] += f64::from(point.y);
                sum[2] += f64::from(point.z);
                count += 1;
            }
            if count > 0 {
                let [x, y, z] = sum.map(|axis| (axis / count as f64) as f32);
                self.follow_target = Some(v3(x, y, z));
            }
        }
        self.follow_target
    }
}
