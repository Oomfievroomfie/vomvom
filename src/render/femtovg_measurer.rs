use crate::render::layout::TextMeasurer;
use crate::render::glyph_cache::measure_text_width;

pub struct SwashMeasurer {
    pub sans_data: &'static [u8],
    pub mono_data: &'static [u8],
}

impl TextMeasurer for SwashMeasurer {
    fn measure_width(&mut self, text: &str, font_size: f32, font_family: &str) -> f32 {
        let data = if font_family == "monospace" { self.mono_data } else { self.sans_data };
        measure_text_width(data, text, font_size)
    }
}
