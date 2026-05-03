//! egui texture cache for per-clip spectrograms.

use std::sync::Arc;

use egui::Context;

use crate::model::Clip;

struct SpecTex {
    key: u64,
    texture: egui::TextureHandle,
}

#[derive(Default)]
pub struct SpecTextureCache {
    entries: Vec<Option<SpecTex>>,
}

impl SpecTextureCache {
    fn spec_cache_key(clip: &Clip) -> Option<u64> {
        let sp = clip.sample.spectrogram.as_ref()?;
        let d = &clip.sample.data;
        Some(
            (Arc::as_ptr(d) as usize as u64).wrapping_mul(0x9E37_79B1_97F4_A7C7)
                ^ (Arc::as_ptr(sp) as usize as u64)
                ^ (d.len() as u64).wrapping_shl(17),
        )
    }

    pub fn sync(&mut self, ctx: &Context, clips: &[Clip]) {
        if self.entries.len() > clips.len() {
            self.entries.truncate(clips.len());
        }
        while self.entries.len() < clips.len() {
            self.entries.push(None);
        }
        for i in 0..clips.len() {
            let clip = &clips[i];
            let Some(spec) = clip.sample.spectrogram.as_ref() else {
                self.entries[i] = None;
                continue;
            };
            let Some(key) = Self::spec_cache_key(clip) else {
                self.entries[i] = None;
                continue;
            };
            let rebuild = match &self.entries[i] {
                None => true,
                Some(e) => e.key != key,
            };
            if rebuild {
                let img =
                    egui::ColorImage::from_rgba_unmultiplied([spec.width, spec.height], &spec.rgba);
                let tex = ctx.load_texture(
                    format!("tinysampler_spec_{i}_{key}"),
                    img,
                    egui::TextureOptions::LINEAR,
                );
                self.entries[i] = Some(SpecTex { key, texture: tex });
            }
        }
    }

    pub fn texture_at(&self, index: usize) -> Option<&egui::TextureHandle> {
        self.entries.get(index).and_then(|e| e.as_ref()).map(|s| &s.texture)
    }
}
