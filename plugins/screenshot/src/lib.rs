//! Формирование итогового изображения для «Скриншотов».
//!
//! Кадр хранится в памяти в координатах виртуального рабочего стола. Модуль
//! вырезает выбранный прямоугольник и наносит аннотации в тех же физических
//! пикселях; панель и маркеры выбора в результат не попадают.

use image::{Rgba, RgbaImage};

/// Точка в координатах виртуального рабочего стола (может быть отрицательной).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// Снимок виртуального рабочего стола с указанием его левого верхнего угла.
#[derive(Debug, Clone)]
pub struct Frame {
    pub origin: Point,
    pub image: RgbaImage,
}

impl Frame {
    pub fn new(origin: Point, image: RgbaImage) -> Self {
        Self { origin, image }
    }
}

/// Выбранный прямоугольник в физических пикселях.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Selection {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// Аннотация редактора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Annotation {
    Arrow {
        from: Point,
        to: Point,
        color: [u8; 4],
        thickness: u32,
    },
    Rectangle {
        rect: Selection,
        color: [u8; 4],
        thickness: u32,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RenderError {
    #[error("выбранная область пуста")]
    EmptySelection,
    #[error("выбранная область выходит за кадр")]
    OutOfFrame,
    #[error("аннотация выходит за пределы кадра")]
    AnnotationOutOfFrame,
}

/// Вырезает область и наносит аннотации. Без аннотаций результат совпадает с
/// исходными пикселями кадра.
pub fn render(
    frame: &Frame,
    selection: Selection,
    annotations: &[Annotation],
) -> Result<RgbaImage, RenderError> {
    if selection.width == 0 || selection.height == 0 {
        return Err(RenderError::EmptySelection);
    }
    let local_x = selection.x - frame.origin.x;
    let local_y = selection.y - frame.origin.y;
    if local_x < 0
        || local_y < 0
        || local_x as u32 + selection.width > frame.image.width()
        || local_y as u32 + selection.height > frame.image.height()
    {
        return Err(RenderError::OutOfFrame);
    }
    let mut result = image::imageops::crop_imm(
        &frame.image,
        local_x as u32,
        local_y as u32,
        selection.width,
        selection.height,
    )
    .to_image();

    for annotation in annotations {
        match *annotation {
            Annotation::Rectangle {
                rect,
                color,
                thickness,
            } => draw_rectangle(&mut result, frame, selection, rect, color, thickness),
            Annotation::Arrow {
                from,
                to,
                color,
                thickness,
            } => draw_arrow(&mut result, frame, selection, from, to, color, thickness),
        }
    }
    Ok(result)
}

fn draw_rectangle(
    image: &mut RgbaImage,
    frame: &Frame,
    selection: Selection,
    rect: Selection,
    color: [u8; 4],
    thickness: u32,
) {
    let left = rect.x - selection.x;
    let top = rect.y - selection.y;
    let right = left + rect.width as i32 - 1;
    let bottom = top + rect.height as i32 - 1;
    let color = Rgba(color);
    let start = Point::new(selection.x + left, selection.y + top);
    let end = Point::new(selection.x + right, selection.y + top);
    draw_line(image, frame, selection, start, end, color, thickness);
    let start = Point::new(selection.x + left, selection.y + bottom);
    let end = Point::new(selection.x + right, selection.y + bottom);
    draw_line(image, frame, selection, start, end, color, thickness);
    let start = Point::new(selection.x + left, selection.y + top);
    let end = Point::new(selection.x + left, selection.y + bottom);
    draw_line(image, frame, selection, start, end, color, thickness);
    let start = Point::new(selection.x + right, selection.y + top);
    let end = Point::new(selection.x + right, selection.y + bottom);
    draw_line(image, frame, selection, start, end, color, thickness);
}

fn draw_arrow(
    image: &mut RgbaImage,
    frame: &Frame,
    selection: Selection,
    from: Point,
    to: Point,
    color: [u8; 4],
    thickness: u32,
) {
    let color = Rgba(color);
    draw_line(image, frame, selection, from, to, color, thickness);
    let dx = (to.x - from.x) as f32;
    let dy = (to.y - from.y) as f32;
    let length = (dx * dx + dy * dy).sqrt();
    if length < 1.0 {
        return;
    }
    let angle = 25f32.to_radians();
    let head = (12.0 + thickness as f32 * 2.0).min(length);
    for direction in [-1.0f32, 1.0] {
        let rotated_x = dx * angle.cos() - dy * angle.sin() * direction;
        let rotated_y = dx * angle.sin() + dy * angle.cos() * direction;
        let norm = (rotated_x * rotated_x + rotated_y * rotated_y).sqrt();
        let end = Point::new(
            to.x - (rotated_x / norm * head) as i32,
            to.y - (rotated_y / norm * head) as i32,
        );
        draw_line(image, frame, selection, to, end, color, thickness);
    }
}

fn draw_line(
    image: &mut RgbaImage,
    frame: &Frame,
    selection: Selection,
    from: Point,
    to: Point,
    color: Rgba<u8>,
    thickness: u32,
) {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let steps = dx.abs().max(dy.abs()).max(1);
    let radius = (thickness.max(1) / 2) as i32;
    for step in 0..=steps {
        let x = from.x + dx * step / steps;
        let y = from.y + dy * step / steps;
        for offset_y in -radius..=radius {
            for offset_x in -radius..=radius {
                paint(image, frame, selection, x + offset_x, y + offset_y, color);
            }
        }
    }
}

fn paint(
    image: &mut RgbaImage,
    frame: &Frame,
    selection: Selection,
    x: i32,
    y: i32,
    color: Rgba<u8>,
) {
    let local_x = x - selection.x;
    let local_y = y - selection.y;
    if local_x < 0
        || local_y < 0
        || local_x as u32 >= image.width()
        || local_y as u32 >= image.height()
    {
        return;
    }
    if x < frame.origin.x || y < frame.origin.y {
        return;
    }
    image.put_pixel(local_x as u32, local_y as u32, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn frame() -> Frame {
        let mut image = RgbaImage::new(100, 80);
        for y in 0..80u32 {
            for x in 0..100u32 {
                image.put_pixel(
                    x,
                    y,
                    Rgba([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8, 255]),
                );
            }
        }
        Frame::new(Point::new(-50, -30), image)
    }

    #[test]
    fn negative_origin_selection_has_expected_size() {
        let frame = frame();
        let selection = Selection::new(-40, -20, 20, 10);
        let result = render(&frame, selection, &[]).expect("рендер");
        assert_eq!(result.dimensions(), (20, 10));
        let expected = frame
            .image
            .get_pixel((-40 - frame.origin.x) as u32, (-20 - frame.origin.y) as u32);
        assert_eq!(result.get_pixel(0, 0), expected);
    }

    #[test]
    fn without_annotations_result_is_pixel_identical() {
        let frame = frame();
        let selection = Selection::new(-45, -25, 30, 25);
        let result = render(&frame, selection, &[]).expect("рендер");
        for y in 0..selection.height {
            for x in 0..selection.width {
                let source = frame.image.get_pixel(
                    (selection.x - frame.origin.x) as u32 + x,
                    (selection.y - frame.origin.y) as u32 + y,
                );
                assert_eq!(result.get_pixel(x, y), source);
            }
        }
    }

    #[test]
    fn undo_removes_only_last_annotation() {
        let frame = frame();
        let selection = Selection::new(-40, -20, 40, 30);
        let arrow = Annotation::Arrow {
            from: Point::new(-35, -15),
            to: Point::new(-5, -5),
            color: [255, 0, 0, 255],
            thickness: 3,
        };
        let rectangle = Annotation::Rectangle {
            rect: Selection::new(-30, -18, 20, 12),
            color: [0, 255, 0, 255],
            thickness: 2,
        };
        let with_arrow = render(&frame, selection, &[arrow]).expect("стрелка");
        let with_both = render(&frame, selection, &[arrow, rectangle]).expect("обе");

        // После отмены рамки пиксель на её границе снова исходный, а стрелка осталась.
        let border = (10u32, 2u32); // левый верхний угол рамки в локальных координатах
        let original = frame.image.get_pixel(
            (selection.x - frame.origin.x) as u32 + border.0,
            (selection.y - frame.origin.y) as u32 + border.1,
        );
        assert_ne!(with_both.get_pixel(border.0, border.1), original);
        assert_eq!(with_arrow.get_pixel(border.0, border.1), original);
        assert!(with_arrow
            .pixels()
            .any(|pixel| pixel == &Rgba([255, 0, 0, 255])));
        assert!(!with_arrow
            .pixels()
            .any(|pixel| pixel == &Rgba([0, 255, 0, 255])));
    }

    #[test]
    fn selection_outside_frame_is_rejected() {
        let frame = frame();
        assert_eq!(
            render(&frame, Selection::new(60, 0, 10, 10), &[]),
            Err(RenderError::OutOfFrame)
        );
        assert_eq!(
            render(&frame, Selection::new(0, 0, 0, 10), &[]),
            Err(RenderError::EmptySelection)
        );
    }
}
