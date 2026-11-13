use crate::stremio_app::constants::{WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH};
use native_windows_derive::NwgPartial;
use native_windows_gui as nwg;

#[derive(Default, NwgPartial)]
pub struct SplashImage {
    #[nwg_resource]
    embed: nwg::EmbedResource,
    #[nwg_resource(size: Some((300,300)), source_embed: Some(&data.embed), source_embed_str: Some("SPLASHIMAGE"))]
    splash_image: nwg::Bitmap,
    #[nwg_layout(spacing: 0, min_size: [WINDOW_MIN_WIDTH as u32, WINDOW_MIN_HEIGHT as u32])]
    grid: nwg::GridLayout,
    #[nwg_control(background_color: Some(Self::BG_COLOR), bitmap: Some(&data.splash_image))]
    #[nwg_layout_item(layout: grid, col: 0, row: 0)]
    splash: nwg::ImageFrame,
}

impl SplashImage {
    const BG_COLOR: [u8; 3] = [27, 17, 38];
    pub fn visible(&self) -> bool {
        self.splash.visible()
    }
    pub fn hide(&self) {
        self.splash.set_visible(false);
    }
    pub fn show(&self) {
        self.splash.set_visible(true);
    }
}
