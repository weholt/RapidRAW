//! Geometry behavior tests: every orientation, flip/rotate composition, and
//! crop/conversion semantics in the documented oriented coordinate system.

use rapidraw_develop::{
    CropRect, PixelRect, apply_coarse_rotation, apply_crop_normalized, apply_flip,
    apply_orientation, apply_pixel_crop, crop_to_source_rect, oriented_dimensions,
    oriented_to_source_pixel,
};
use rawler::decoders::Orientation;

/// 3x2 image with visually distinct pixels: values 1..=6 in the red channel.
fn test_image() -> rapidraw_develop::LinearImage {
    rapidraw_develop::LinearImage::from_fn(3, 2, |x, y| {
        let v = (y * 3 + x) as f32 + 1.0;
        [v, v * 10.0, v * 100.0]
    })
}

fn red_channel(image: &rapidraw_develop::LinearImage) -> Vec<f32> {
    (0..image.height())
        .flat_map(|y| (0..image.width()).map(move |x| image.pixel(x, y)[0]))
        .collect()
}

#[test]
fn normal_orientation_is_identity() {
    let image = test_image();
    let out = apply_orientation(&image, Orientation::Normal);
    assert_eq!(out.dimensions(), (3, 2));
    assert_eq!(red_channel(&out), vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
}

#[test]
fn horizontal_flip_mirrors_rows() {
    let image = test_image();
    let out = apply_orientation(&image, Orientation::HorizontalFlip);
    assert_eq!(out.dimensions(), (3, 2));
    assert_eq!(red_channel(&out), vec![3.0, 2.0, 1.0, 6.0, 5.0, 4.0]);
}

#[test]
fn rotate180_reverses_all_pixels() {
    let image = test_image();
    let out = apply_orientation(&image, Orientation::Rotate180);
    assert_eq!(out.dimensions(), (3, 2));
    assert_eq!(red_channel(&out), vec![6.0, 5.0, 4.0, 3.0, 2.0, 1.0]);
}

#[test]
fn vertical_flip_mirrors_columns() {
    let image = test_image();
    let out = apply_orientation(&image, Orientation::VerticalFlip);
    assert_eq!(out.dimensions(), (3, 2));
    assert_eq!(red_channel(&out), vec![4.0, 5.0, 6.0, 1.0, 2.0, 3.0]);
}

#[test]
fn rotate90_swaps_dimensions_clockwise() {
    let image = test_image();
    let out = apply_orientation(&image, Orientation::Rotate90);
    assert_eq!(out.dimensions(), (2, 3));
    // src(0,0)->dst(1,0); src(1,0)->dst(1,1); src(2,0)->dst(1,2)
    // src(0,1)->dst(0,0); src(1,1)->dst(0,1); src(2,1)->dst(0,2)
    assert_eq!(red_channel(&out), vec![4.0, 1.0, 5.0, 2.0, 6.0, 3.0]);
}

#[test]
fn rotate270_swaps_dimensions_counterclockwise() {
    let image = test_image();
    let out = apply_orientation(&image, Orientation::Rotate270);
    assert_eq!(out.dimensions(), (2, 3));
    assert_eq!(red_channel(&out), vec![3.0, 6.0, 2.0, 5.0, 1.0, 4.0]);
}

#[test]
fn transpose_is_rotate90_then_fliph() {
    let image = test_image();
    let out = apply_orientation(&image, Orientation::Transpose);
    assert_eq!(out.dimensions(), (2, 3));
    assert_eq!(red_channel(&out), vec![1.0, 4.0, 2.0, 5.0, 3.0, 6.0]);
}

#[test]
fn transverse_is_rotate270_then_fliph() {
    let image = test_image();
    let out = apply_orientation(&image, Orientation::Transverse);
    assert_eq!(out.dimensions(), (2, 3));
    assert_eq!(red_channel(&out), vec![6.0, 3.0, 5.0, 2.0, 4.0, 1.0]);
}

#[test]
fn unknown_orientation_is_identity() {
    let image = test_image();
    let out = apply_orientation(&image, Orientation::Unknown);
    assert_eq!(red_channel(&out), red_channel(&image));
}

#[test]
fn oriented_dimensions_swap_for_quarter_turns() {
    for orientation in [
        Orientation::Normal,
        Orientation::Unknown,
        Orientation::HorizontalFlip,
        Orientation::Rotate180,
        Orientation::VerticalFlip,
    ] {
        assert_eq!(
            oriented_dimensions(640, 480, orientation),
            (640, 480),
            "{orientation:?}"
        );
    }
    for orientation in [
        Orientation::Rotate90,
        Orientation::Rotate270,
        Orientation::Transpose,
        Orientation::Transverse,
    ] {
        assert_eq!(
            oriented_dimensions(640, 480, orientation),
            (480, 640),
            "{orientation:?}"
        );
    }
}

#[test]
fn oriented_to_source_pixel_round_trips_every_orientation() {
    let (w, h) = (3u32, 2u32);
    let image = test_image();
    for orientation in [
        Orientation::Normal,
        Orientation::Unknown,
        Orientation::HorizontalFlip,
        Orientation::Rotate180,
        Orientation::VerticalFlip,
        Orientation::Rotate90,
        Orientation::Rotate270,
        Orientation::Transpose,
        Orientation::Transverse,
    ] {
        let oriented = apply_orientation(&image, orientation);
        let (ow, oh) = oriented_dimensions(w, h, orientation);
        assert_eq!(oriented.dimensions(), (ow, oh), "{orientation:?}");
        for y in 0..oh {
            for x in 0..ow {
                let (sx, sy) = oriented_to_source_pixel(x, y, w, h, orientation)
                    .unwrap_or_else(|| panic!("{orientation:?}: ({x},{y}) unmapped"));
                assert_eq!(
                    oriented.pixel(x, y),
                    image.pixel(sx, sy),
                    "{orientation:?}: oriented ({x},{y}) must show source ({sx},{sy})"
                );
            }
        }
    }
}

#[test]
fn coordinate_conversion_rejects_out_of_frame_coordinates() {
    assert!(oriented_to_source_pixel(3, 0, 3, 2, Orientation::Normal).is_none());
    assert!(oriented_to_source_pixel(0, 2, 3, 2, Orientation::Normal).is_none());
    // Rotate90 oriented frame is 2x3: (2,0) is out of frame, (1,2) is not.
    assert!(oriented_to_source_pixel(2, 0, 3, 2, Orientation::Rotate90).is_none());
    assert!(oriented_to_source_pixel(1, 2, 3, 2, Orientation::Rotate90).is_some());
}

#[test]
fn flip_composition_horizontal_then_vertical_equals_rotate180() {
    let image = test_image();
    let both = apply_flip(&image, true, true);
    let rot = apply_orientation(&image, Orientation::Rotate180);
    assert_eq!(red_channel(&both), red_channel(&rot));
}

#[test]
fn apply_flip_noop_returns_same_pixels() {
    let image = test_image();
    let out = apply_flip(&image, false, false);
    assert_eq!(red_channel(&out), red_channel(&image));
}

#[test]
fn coarse_rotation_matches_orientation_steps() {
    let image = test_image();
    assert_eq!(
        red_channel(&apply_coarse_rotation(&image, 1)),
        red_channel(&apply_orientation(&image, Orientation::Rotate90))
    );
    assert_eq!(
        red_channel(&apply_coarse_rotation(&image, 2)),
        red_channel(&apply_orientation(&image, Orientation::Rotate180))
    );
    assert_eq!(
        red_channel(&apply_coarse_rotation(&image, 3)),
        red_channel(&apply_orientation(&image, Orientation::Rotate270))
    );
    assert_eq!(
        red_channel(&apply_coarse_rotation(&image, 0)),
        red_channel(&image)
    );
    assert_eq!(
        red_channel(&apply_coarse_rotation(&image, 7)),
        red_channel(&image)
    );
}

#[test]
fn pixel_crop_extracts_the_requested_region() {
    let image = test_image();
    let out = apply_pixel_crop(&image, 1.0, 0.0, 2.0, 1.0);
    assert_eq!(out.dimensions(), (2, 1));
    assert_eq!(red_channel(&out), vec![2.0, 3.0]);
}

#[test]
fn pixel_crop_clamps_to_image_bounds_like_the_host() {
    let image = test_image();
    // Requested region runs past the right/bottom edges: clamped, not error.
    let out = apply_pixel_crop(&image, 1.0, 1.0, 50.0, 50.0);
    assert_eq!(out.dimensions(), (2, 1));
    assert_eq!(red_channel(&out), vec![5.0, 6.0]);
}

#[test]
fn pixel_crop_starting_outside_returns_image_unchanged() {
    let image = test_image();
    let out = apply_pixel_crop(&image, 5.0, 0.0, 1.0, 1.0);
    assert_eq!(out.dimensions(), (3, 2));
    assert_eq!(red_channel(&out), red_channel(&image));
}

#[test]
fn degenerate_crop_returns_image_unchanged() {
    let image = test_image();
    let out = apply_pixel_crop(&image, 0.0, 0.0, 0.0, 1.0);
    assert_eq!(out.dimensions(), (3, 2));
    assert_eq!(red_channel(&out), red_channel(&image));
}

#[test]
fn full_frame_crop_is_a_noop() {
    let image = test_image();
    let out = apply_pixel_crop(&image, 0.0, 0.0, 3.0, 2.0);
    assert_eq!(out.dimensions(), (3, 2));
    assert_eq!(red_channel(&out), red_channel(&image));
}

#[test]
fn pixel_crop_coordinates_round_like_the_host() {
    let image = test_image();
    // Host rounds crop components: 0.4 -> 0, 1.6 -> 2, 1.4 -> 1.
    let at_origin = apply_pixel_crop(&image, 0.4, 0.0, 1.6, 2.0);
    assert_eq!(at_origin.dimensions(), (2, 2));
    assert_eq!(red_channel(&at_origin), vec![1.0, 2.0, 4.0, 5.0]);
    let at_x = apply_pixel_crop(&image, 1.4, 0.0, 1.6, 2.0);
    assert_eq!(at_x.dimensions(), (2, 2));
    assert_eq!(red_channel(&at_x), vec![2.0, 3.0, 5.0, 6.0]);
}

#[test]
fn normalized_crop_scales_with_oriented_dimensions() {
    let image = test_image();
    let crop = CropRect {
        x: 1.0 / 3.0,
        y: 0.0,
        width: 2.0 / 3.0,
        height: 0.5,
    };
    let out = apply_crop_normalized(&image, &crop).expect("valid crop");
    assert_eq!(out.dimensions(), (2, 1));
    assert_eq!(red_channel(&out), vec![2.0, 3.0]);
}

#[test]
fn normalized_crop_rejects_invalid_rectangles() {
    let image = test_image();
    let invalid = [
        CropRect {
            x: 0.5,
            y: 0.0,
            width: 0.75,
            height: 0.5,
        },
        CropRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.5,
        },
        CropRect {
            x: f64::NAN,
            y: 0.0,
            width: 0.5,
            height: 0.5,
        },
        CropRect {
            x: -0.1,
            y: 0.0,
            width: 0.5,
            height: 0.5,
        },
    ];
    for crop in &invalid {
        let err = apply_crop_normalized(&image, crop).expect_err("crop must be rejected");
        assert!(
            matches!(err, rapidraw_develop::DevelopError::Geometry(_)),
            "{err}"
        );
    }
}

#[test]
fn crop_to_source_rect_maps_full_frame_identity_orientation() {
    let crop = CropRect {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };
    let rect = crop_to_source_rect(&crop, 3, 2, Orientation::Normal).expect("valid");
    assert_eq!(
        rect,
        PixelRect {
            x: 0,
            y: 0,
            width: 3,
            height: 2
        }
    );
}

#[test]
fn crop_to_source_rect_rotates_with_the_orientation() {
    // Right half of a Rotate90-oriented 2x3 frame (source 3x2) comes from the
    // top row of the source: oriented x in [1, 2) maps to source y = 0.
    let crop = CropRect {
        x: 0.5,
        y: 0.0,
        width: 0.5,
        height: 1.0,
    };
    let rect = crop_to_source_rect(&crop, 3, 2, Orientation::Rotate90).expect("valid");
    assert_eq!(
        rect,
        PixelRect {
            x: 0,
            y: 0,
            width: 3,
            height: 1
        }
    );
}

#[test]
fn crop_to_source_rect_rejects_invalid_input() {
    let crop = CropRect {
        x: 0.0,
        y: 0.0,
        width: 2.0,
        height: 1.0,
    };
    let err = crop_to_source_rect(&crop, 3, 2, Orientation::Normal).expect_err("must reject");
    assert!(matches!(err, rapidraw_develop::DevelopError::Geometry(_)));
    let err = crop_to_source_rect(&crop, 0, 0, Orientation::Normal).expect_err("must reject");
    assert!(matches!(err, rapidraw_develop::DevelopError::Geometry(_)));
}
