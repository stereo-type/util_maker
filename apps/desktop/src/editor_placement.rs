//! Размещение окна редактора снимка на активном мониторе.
//!
//! Выделение задано в физических пикселях виртуального рабочего стола. Окно
//! центрируется на мониторе с наибольшей площадью пересечения с выделением,
//! поэтому не пересекает стык мониторов и не уезжает на соседний экран.

use util_maker_sdk::{FrameRegion, MonitorGeometry};

/// Желаемый внешний размер окна редактора.
pub const EDITOR_WIDTH: u32 = 1100;
/// Желаемая внешняя высота окна редактора.
pub const EDITOR_HEIGHT: u32 = 760;
/// Нейтральный отступ от краёв монитора.
pub const EDITOR_MARGIN: u32 = 32;

/// Положение и размер окна редактора в физических пикселях рабочего стола.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorPlacement {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Центрирует редактор на мониторе с наибольшей площадью пересечения.
///
/// При равной площади предпочитается основной монитор. Если геометрия пуста
/// или выделение не пересекает ни один монитор, используется `fallback`.
pub fn place_editor(
    selection: FrameRegion,
    monitors: &[MonitorGeometry],
    fallback: FrameRegion,
) -> EditorPlacement {
    let monitor = active_monitor(selection, monitors)
        .map(|monitor| monitor.rect)
        .unwrap_or(fallback);
    center_in(monitor)
}

fn active_monitor(
    selection: FrameRegion,
    monitors: &[MonitorGeometry],
) -> Option<&MonitorGeometry> {
    let mut best: Option<(&MonitorGeometry, u64)> = None;
    for monitor in monitors {
        let area = intersection_area(selection, monitor.rect);
        if area == 0 {
            continue;
        }
        let replace = match best {
            None => true,
            Some((current, current_area)) => {
                area > current_area || (area == current_area && monitor.primary && !current.primary)
            }
        };
        if replace {
            best = Some((monitor, area));
        }
    }
    if let Some((monitor, _)) = best {
        return Some(monitor);
    }

    let center = selection_center(selection);
    monitors
        .iter()
        .find(|monitor| contains_point(monitor.rect, center))
        .or_else(|| monitors.iter().find(|monitor| monitor.primary))
        .or_else(|| monitors.first())
}

fn center_in(rect: FrameRegion) -> EditorPlacement {
    let available_width = rect
        .width
        .saturating_sub(EDITOR_MARGIN.saturating_mul(2))
        .max(1);
    let available_height = rect
        .height
        .saturating_sub(EDITOR_MARGIN.saturating_mul(2))
        .max(1);
    let width = EDITOR_WIDTH.min(available_width);
    let height = EDITOR_HEIGHT.min(available_height);
    let x = i64::from(rect.x) + (i64::from(rect.width) - i64::from(width)) / 2;
    let y = i64::from(rect.y) + (i64::from(rect.height) - i64::from(height)) / 2;
    EditorPlacement {
        x: x as i32,
        y: y as i32,
        width,
        height,
    }
}

fn selection_center(selection: FrameRegion) -> (i64, i64) {
    (
        i64::from(selection.x) + i64::from(selection.width) / 2,
        i64::from(selection.y) + i64::from(selection.height) / 2,
    )
}

fn contains_point(rect: FrameRegion, point: (i64, i64)) -> bool {
    let (x, y) = point;
    x >= i64::from(rect.x)
        && y >= i64::from(rect.y)
        && x < i64::from(rect.x) + i64::from(rect.width)
        && y < i64::from(rect.y) + i64::from(rect.height)
}

fn intersection_area(left: FrameRegion, right: FrameRegion) -> u64 {
    let x1 = i64::from(left.x).max(i64::from(right.x));
    let y1 = i64::from(left.y).max(i64::from(right.y));
    let x2 = (i64::from(left.x) + i64::from(left.width))
        .min(i64::from(right.x) + i64::from(right.width));
    let y2 = (i64::from(left.y) + i64::from(left.height))
        .min(i64::from(right.y) + i64::from(right.height));
    if x2 <= x1 || y2 <= y1 {
        return 0;
    }
    ((x2 - x1) as u64) * ((y2 - y1) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        primary: bool,
        scale: f64,
    ) -> MonitorGeometry {
        MonitorGeometry {
            rect: FrameRegion {
                x,
                y,
                width,
                height,
            },
            scale_factor: scale,
            primary,
        }
    }

    fn two_monitors() -> Vec<MonitorGeometry> {
        vec![
            monitor(-2560, 0, 2560, 1440, false, 1.0),
            monitor(0, 0, 3440, 1440, true, 1.5),
        ]
    }

    #[test]
    fn centers_editor_on_left_monitor_with_negative_origin() {
        let placement = place_editor(
            FrameRegion {
                x: -2200,
                y: 300,
                width: 500,
                height: 400,
            },
            &two_monitors(),
            FrameRegion {
                x: -2560,
                y: 0,
                width: 6000,
                height: 1440,
            },
        );
        assert_eq!(
            placement,
            EditorPlacement {
                x: -1830,
                y: 340,
                width: 1100,
                height: 760,
            }
        );
    }

    #[test]
    fn selection_size_does_not_move_editor_within_monitor() {
        let fallback = FrameRegion {
            x: -2560,
            y: 0,
            width: 6000,
            height: 1440,
        };
        let small = place_editor(
            FrameRegion {
                x: -2400,
                y: 100,
                width: 50,
                height: 50,
            },
            &two_monitors(),
            fallback,
        );
        let large = place_editor(
            FrameRegion {
                x: -1500,
                y: 500,
                width: 1200,
                height: 800,
            },
            &two_monitors(),
            fallback,
        );
        assert_eq!(small, large);
    }

    #[test]
    fn chooses_monitor_with_larger_selection_area_at_seam() {
        let placement = place_editor(
            FrameRegion {
                x: -200,
                y: 200,
                width: 600,
                height: 500,
            },
            &two_monitors(),
            FrameRegion {
                x: -2560,
                y: 0,
                width: 6000,
                height: 1440,
            },
        );
        assert_eq!(
            placement,
            EditorPlacement {
                x: 1170,
                y: 340,
                width: 1100,
                height: 760,
            }
        );
    }

    #[test]
    fn primary_monitor_wins_tie_at_seam() {
        let placement = place_editor(
            FrameRegion {
                x: -100,
                y: 200,
                width: 200,
                height: 200,
            },
            &two_monitors(),
            FrameRegion {
                x: -2560,
                y: 0,
                width: 6000,
                height: 1440,
            },
        );
        assert_eq!(placement.x, 1170);
    }

    #[test]
    fn small_monitor_fits_editor_with_margins() {
        let placement = place_editor(
            FrameRegion {
                x: 20,
                y: 20,
                width: 200,
                height: 100,
            },
            &[monitor(0, 0, 800, 600, true, 1.0)],
            FrameRegion {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            },
        );
        assert_eq!(
            placement,
            EditorPlacement {
                x: 32,
                y: 32,
                width: 736,
                height: 536,
            }
        );
    }

    #[test]
    fn missing_geometry_falls_back_to_captured_frame() {
        let placement = place_editor(
            FrameRegion {
                x: -100,
                y: -100,
                width: 20,
                height: 20,
            },
            &[],
            FrameRegion {
                x: -1920,
                y: -200,
                width: 1920,
                height: 1080,
            },
        );
        assert_eq!(
            placement,
            EditorPlacement {
                x: -1510,
                y: -40,
                width: 1100,
                height: 760,
            }
        );
    }

    #[test]
    fn selection_outside_monitors_uses_primary_monitor() {
        let placement = place_editor(
            FrameRegion {
                x: 9000,
                y: 9000,
                width: 100,
                height: 100,
            },
            &two_monitors(),
            FrameRegion {
                x: -2560,
                y: 0,
                width: 6000,
                height: 1440,
            },
        );
        assert_eq!(placement.x, 1170);
        assert_eq!(placement.y, 340);
    }
}
