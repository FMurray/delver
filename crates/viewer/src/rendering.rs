use crate::app::Viewer;
use crate::utils;
use eframe::{egui, epaint};
use pdfium_render::prelude::*;

/// Render the PDF view with all visualizations
pub fn render_pdf_view(viewer: &mut Viewer, ui: &mut egui::Ui) {
    if let Some(doc) = &viewer.pdf_document {
        if let Some(page) = doc.pages().get(viewer.current_page as u16).ok() {
            let texture =
                render_page_to_texture(&page, 1.0, ui.ctx(), egui::TextureOptions::LINEAR);
            let (rect, _transform) = utils::calculate_pdf_view_rect(viewer, ui, &texture);
            utils::draw_pdf_page(ui, &texture, rect);
        }
    }
}

fn render_page_to_texture(
    page: &PdfPage,
    pixels_per_point: f32,
    ctx: &egui::Context,
    texture_options: egui::TextureOptions,
) -> egui::TextureHandle {
    let width = (page.width().value * pixels_per_point) as i32;
    let height = (page.height().value * pixels_per_point) as i32;
    let image_buffer = page
        .render_with_config(&PdfRenderConfig::new().set_target_size(width, height))
        .unwrap()
        .as_image()
        .to_rgba8();

    let color_image = egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        &image_buffer.into_raw(),
    );

    ctx.load_texture(
        "pdf_page",
        egui::ImageData::from(color_image),
        texture_options,
    )
}
