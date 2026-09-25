use base64::{Engine as _, engine::general_purpose::STANDARD};
use nix::{
    errno::Errno,
    fcntl::{FcntlArg, OFlag, fcntl},
    pty::Winsize,
    sys::wait::{WaitPidFlag, waitpid},
    unistd::Pid,
};
use rustmux::{
    graphics_composite::{ImageLayer, compose_image_layers},
    graphics_snapshot::{BACKGROUND_Z_BOUNDARY, ImageBand, SnapshotError, compose_store_snapshot},
    graphics_store::{
        CellPixelSize, PixelRect, PixelSize, PlacementGeometry, PlacementSizing, SignedPixelPoint,
    },
    layout::SplitAxis,
    pane::Pane,
    pane_set::PaneSet,
    window::Windows,
};
use std::{
    io::{Read, Write},
    thread,
    time::{Duration, Instant},
};

fn text(pane: &Pane) -> String {
    (0..pane.screen().dimensions().0)
        .flat_map(|row| pane.screen().row(row).unwrap())
        .map(|cell| cell.character)
        .collect()
}

fn placement_geometry(pane: &Pane, id: u32) -> PlacementGeometry {
    pane.image_store()
        .placements()
        .find(|placement| placement.placement_id == Some(id))
        .unwrap()
        .geometry
        .unwrap()
}

fn read_once(pane: &mut Pane) -> bool {
    let mut buffer = [0; 4096];
    match pane.shell_mut().read(&mut buffer) {
        Ok(0) => {
            pane.finish_output();
            false
        }
        Ok(n) => {
            pane.process_output(&buffer[..n], &mut |_| panic!("unexpected child query"));
            true
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            false
        }
        Err(error) => panic!("PTY read: {error}"),
    }
}

fn until(pane: &mut Pane, condition: impl Fn(&Pane) -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !condition(pane) {
        read_once(pane);
        assert!(Instant::now() < end, "output timed out: {}", text(pane));
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn kitty_transfer_callbacks_and_partial_commands_stay_with_their_pane() {
    let mut left = Pane::spawn("/bin/sh", 6, 40).unwrap();
    let mut right = Pane::spawn("/bin/sh", 6, 40).unwrap();
    let mut left_images = Vec::new();
    let mut right_images = Vec::new();
    left.process_output_with_graphics(
        b"left\x1b_Ga=T,f=32,s=1,v=1,i=7,m=1;AQ",
        &mut |_| {},
        &mut |image| left_images.push(image),
    );
    right.process_output_with_graphics(
        b"right\x1b_Ga=T,f=24,s=1,v=1,i=7;BAUG\x1b\\",
        &mut |_| {},
        &mut |image| right_images.push(image),
    );
    left.process_output_with_graphics(
        b"ID\x1b\\middle\x1b_Gm=0;BA==\x1b\\end",
        &mut |_| {},
        &mut |image| left_images.push(image),
    );
    assert_eq!(left_images.len(), 1);
    assert_eq!(left_images[0].data, [1, 2, 3, 4]);
    assert_eq!(left_images[0].control(b'i'), Some(b"7".as_slice()));
    assert_eq!(right_images.len(), 1);
    assert_eq!(right_images[0].data, [4, 5, 6]);
    assert!(text(&left).starts_with("leftmiddleend"), "{}", text(&left));
    assert!(text(&right).starts_with("right"), "{}", text(&right));

    // The ordinary runtime path does not retain or surface image data.
    left.process_output(b"\x1b_Gf=100;U0VDUkVU\x1b\\tail", &mut |_| {});
    assert!(!text(&left).contains("SECRET"));
    assert!(text(&left).contains("tail"));

    // Switching to the non-capturing path abandons a partial transfer.
    left.process_output_with_graphics(b"\x1b_Gf=100,m=1;QUJD\x1b\\", &mut |_| {}, &mut |image| {
        left_images.push(image)
    });
    left.process_output(b"plain", &mut |_| {});
    left.process_output_with_graphics(b"\x1b_Gm=0;RA==\x1b\\", &mut |_| {}, &mut |image| {
        left_images.push(image)
    });
    assert_eq!(left_images.len(), 1);
}

#[test]
fn kitty_image_data_is_opt_in_and_isolated_per_pane() {
    let mut left = Pane::spawn("/bin/sh", 6, 40).unwrap();
    let mut right = Pane::spawn("/bin/sh", 6, 40).unwrap();
    left.process_output_with_image_store(b"\x1b_Ga=T,f=100,i=7,m=1;QUJD\x1b\\", &mut |_| {});
    right.process_output_with_image_store(b"\x1b_Ga=t,f=100,i=7;RA==\x1b\\", &mut |_| {});
    left.process_output_with_image_store(b"\x1b_Gm=0;RA==\x1b\\", &mut |_| {});
    assert_eq!(left.image_store().get(7).unwrap().data, b"ABCD");
    assert_eq!(right.image_store().get(7).unwrap().data, b"D");

    left.image_store_mut().remove(7);
    assert!(left.image_store().is_empty());
    assert_eq!(right.image_store().get(7).unwrap().data, b"D");
    right.process_output(b"\x1b_Ga=t,f=100,i=8;QQ==\x1b\\", &mut |_| {});
    assert!(right.image_store().get(8).is_none());
    right.finish_output();
    assert!(right.image_store().is_empty());
}

#[test]
fn kitty_placement_lifecycle_commands_stay_with_their_pane() {
    let mut left = Pane::spawn("/bin/sh", 6, 40).unwrap();
    let mut right = Pane::spawn("/bin/sh", 6, 40).unwrap();
    for pane in [&mut left, &mut right] {
        pane.process_output_with_image_store(b"\x1b_Ga=T,f=100,i=7,p=1;QQ==\x1b\\", &mut |_| {});
    }
    left.process_output_with_image_store(b"\x1b_Ga=p,i=7,p=2\x1b\\", &mut |_| {});
    left.process_output_with_image_store(b"\x1b_Ga=d,d=I,i=7,p=1\x1b\\", &mut |_| {});
    assert_eq!(left.image_store().placements().count(), 1);
    assert_eq!(right.image_store().placements().count(), 1);
    assert!(left.image_store().get(7).is_some());
    left.process_output_with_image_store(b"\x1b_Ga=d,d=I,i=7", &mut |_| {});
    left.process_output_with_image_store(b"\x1b\\", &mut |_| {});
    assert!(left.image_store().get(7).is_none());
    assert!(right.image_store().get(7).is_some());
}

#[test]
fn kitty_delete_aborts_an_unfinished_transfer() {
    let mut pane = Pane::spawn("/bin/sh", 6, 40).unwrap();
    pane.process_output_with_image_store(b"\x1b_Ga=T,f=100,i=7,m=1;QUJD\x1b\\", &mut |_| {});
    pane.process_output_with_image_store(b"\x1b_Ga=d,d=I,i=7\x1b\\", &mut |_| {});
    pane.process_output_with_image_store(b"\x1b_Gm=0;RA==\x1b\\", &mut |_| {});
    assert!(pane.image_store().is_empty());
    assert_eq!(pane.image_store().placements().count(), 0);
}

#[test]
fn kitty_stored_raw_image_decodes_on_demand() {
    let mut pane = Pane::spawn("/bin/sh", 6, 40).unwrap();
    pane.process_output_with_image_store(b"\x1b_Ga=t,f=24,i=7,s=1,v=1;AQID\x1b\\", &mut |_| {});
    let decoded = pane.image_store().get(7).unwrap().decode_rgba().unwrap();
    assert_eq!((decoded.width, decoded.height), (1, 1));
    assert_eq!(decoded.pixels, [1, 2, 3, 255]);
}

#[test]
fn kitty_placement_anchor_uses_cursor_at_final_chunk_and_put() {
    let mut pane = Pane::spawn("/bin/sh", 6, 40).unwrap();
    pane.process_output_with_image_store(
        b"ab\x1b_Ga=T,f=24,i=7,s=1,v=1,p=1,c=2,r=1,C=1,m=1;AQID\x1b\\",
        &mut |_| {},
    );
    pane.process_output_with_image_store(b"cd\x1b_Gm=0;\x1b\\", &mut |_| {});
    let first = pane.image_store().placements().next().unwrap();
    let geometry = first.geometry.unwrap();
    assert_eq!((geometry.anchor.row, geometry.anchor.column), (0, 4));
    assert_eq!((geometry.columns, geometry.rows), (Some(2), Some(1)));
    assert!(geometry.cursor_stays);

    pane.process_output_with_image_store(
        b"\x1b[?1049h\x1b[3;4H\x1b_Ga=p,i=7,p=2,c=1,r=2,z=-1\x1b\\",
        &mut |_| {},
    );
    let second = pane
        .image_store()
        .placements()
        .find(|placement| placement.placement_id == Some(2))
        .unwrap();
    let geometry = second.geometry.unwrap();
    assert_eq!((geometry.anchor.row, geometry.anchor.column), (2, 3));
    assert!(geometry.anchor.alternate);
    assert_eq!(geometry.z_index, -1);
}

#[test]
fn kitty_explicit_cell_extent_moves_cursor_before_following_text() {
    let mut pane = Pane::spawn("/bin/sh", 6, 20).unwrap();
    pane.process_output_with_image_store(
        b"ab\x1b_Ga=T,f=100,i=7,p=1,c=2,r=1;QQ==\x1b\\X\x1b_Ga=p,i=7,p=2,c=3,r=2\x1b\\Y",
        &mut |_| {},
    );
    assert_eq!(placement_geometry(&pane, 1).anchor.column, 2);
    assert_eq!(
        placement_geometry(&pane, 2).anchor,
        rustmux::graphics_store::CellAnchor {
            row: 1,
            column: 5,
            alternate: false,
        }
    );
    assert_eq!(pane.screen().cursor(), (3, 9));
    assert_eq!(pane.screen().row(1).unwrap()[4].character, 'X');
    assert_eq!(pane.screen().row(3).unwrap()[8].character, 'Y');
    pane.process_output(b"\x1b_Ga=p,i=7,c=2,r=1\x1b\\", &mut |_| {});
    assert_eq!(pane.screen().cursor(), (3, 9));
}

#[test]
fn kitty_cursor_stays_for_opt_out_unknown_extent_and_failed_placement() {
    let mut pane = Pane::spawn("/bin/sh", 6, 20).unwrap();
    pane.process_output_with_image_store(
        b"ab\x1b_Ga=T,f=100,i=7,c=2,r=1,C=1;QQ==\x1b\\X",
        &mut |_| {},
    );
    assert_eq!(pane.screen().cursor(), (0, 3));
    pane.process_output_with_image_store(
        b"\x1b_Ga=p,i=7,p=2,c=2\x1b\\\x1b_Ga=p,i=7,c=2,r=1,C=1\x1b\\\x1b_Ga=p,i=999,c=2,r=1\x1b\\\x1b_Ga=p,i=7,c=bad,r=1\x1b\\Y",
        &mut |_| {},
    );
    assert_eq!(pane.screen().cursor(), (0, 4));
    assert_eq!(pane.screen().row(0).unwrap()[3].character, 'Y');
    assert_eq!(placement_geometry(&pane, 2).columns, Some(2));
    assert_eq!(placement_geometry(&pane, 2).rows, None);
}

#[test]
fn kitty_final_chunk_uses_final_cursor_then_clamps_out_of_bounds_motion() {
    let mut pane = Pane::spawn("/bin/sh", 3, 5).unwrap();
    pane.process_output_with_image_store(
        b"\x1b_Ga=T,f=100,i=7,p=1,c=2,r=1,m=1;QQ==\x1b\\ab",
        &mut |_| {},
    );
    assert_eq!(pane.screen().cursor(), (0, 2));
    pane.process_output_with_image_store(b"\x1b_Gm=0;Qg==\x1b\\", &mut |_| {});
    assert_eq!(placement_geometry(&pane, 1).anchor.column, 2);
    assert_eq!(pane.screen().cursor(), (1, 4));
    pane.process_output_with_image_store(b"\x1b_Ga=p,i=7,p=2,c=99,r=99\x1b\\", &mut |_| {});
    assert_eq!(pane.screen().cursor(), (2, 4));
}

#[test]
fn kitty_supplied_cell_pixels_infer_raw_extents_and_cursor_motion() {
    let mut pane = Pane::spawn("/bin/sh", 8, 20).unwrap();
    let cell = CellPixelSize::new(2, 1).unwrap();
    let image = format!(
        "\x1b_Ga=T,f=24,s=4,v=2,i=7,p=1;{}\x1b\\",
        STANDARD.encode([0; 24])
    );
    pane.process_output_with_image_store_sized(image.as_bytes(), &mut |_| {}, cell);
    assert_eq!(
        (
            placement_geometry(&pane, 1).columns,
            placement_geometry(&pane, 1).rows
        ),
        (Some(2), Some(2))
    );
    assert_eq!(pane.screen().cursor(), (2, 2));
    assert_eq!(
        placement_geometry(&pane, 1).sizing,
        PlacementSizing::Natural
    );

    pane.process_output_with_image_store_sized(
        b"\x1b_Ga=p,i=7,p=2,c=4,C=1\x1b\\\x1b_Ga=p,i=7,p=3,r=3\x1b\\",
        &mut |_| {},
        cell,
    );
    assert_eq!(placement_geometry(&pane, 2).rows, Some(4));
    assert_eq!(
        placement_geometry(&pane, 2).sizing,
        PlacementSizing::FitWidth
    );
    assert_eq!(placement_geometry(&pane, 3).columns, Some(3));
    assert_eq!(
        placement_geometry(&pane, 3).sizing,
        PlacementSizing::FitHeight
    );
    assert_eq!(pane.screen().cursor(), (5, 5));

    pane.process_output_with_image_store(b"\x1b_Ga=p,i=7,p=4\x1b\\", &mut |_| {});
    assert_eq!(placement_geometry(&pane, 4).columns, None);
    assert_eq!(
        placement_geometry(&pane, 4).sizing,
        PlacementSizing::Natural
    );
    assert_eq!(pane.screen().cursor(), (5, 5));

    pane.process_output_with_image_store(b"\x1b_Ga=t,f=24,s=1,v=1,i=8;AAAA\x1b\\", &mut |_| {});
    pane.process_output_with_image_store_sized(b"\x1b_Ga=p,i=8,p=5\x1b\\", &mut |_| {}, cell);
    assert_eq!(placement_geometry(&pane, 5).columns, Some(1));
    assert_eq!(placement_geometry(&pane, 5).rows, Some(1));
    assert_eq!(
        placement_geometry(&pane, 5).sizing,
        PlacementSizing::Natural
    );
    assert_eq!(pane.screen().cursor(), (6, 6));

    pane.process_output_with_image_store_sized(
        b"\x1b_Ga=p,i=7,p=6,c=3,r=2,C=1\x1b\\\x1b_Ga=p,i=7,p=7,c=0,r=0,C=1\x1b\\",
        &mut |_| {},
        cell,
    );
    assert_eq!(placement_geometry(&pane, 6).sizing, PlacementSizing::FitBox);
    let decoded = pane.image_store().get(7).unwrap().decode_rgba().unwrap();
    let pixel_layout = placement_geometry(&pane, 6)
        .pixel_layout(decoded.width, decoded.height, cell)
        .unwrap();
    assert_eq!(
        pixel_layout.cell_bounds,
        PixelSize {
            width: 6,
            height: 2
        }
    );
    let resampled = decoded.resample_placement(pixel_layout).unwrap();
    assert_eq!(resampled.destination, pixel_layout.destination);
    assert_eq!(resampled.pixels.len(), 4 * 2 * 4);
    assert!(
        resampled
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|rgba| rgba[3] == 255)
    );
    let anchor = placement_geometry(&pane, 6).pixel_anchor(cell).unwrap();
    assert_eq!(anchor, SignedPixelPoint { x: 12, y: 6 });
    let clipped = resampled
        .clip_to_viewport(
            anchor,
            PixelSize {
                width: 14,
                height: 7,
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        clipped.destination,
        PixelRect {
            x: 13,
            y: 6,
            width: 1,
            height: 1,
        }
    );
    assert_eq!(clipped.pixels, [0, 0, 0, 255]);
    assert_eq!(
        pixel_layout.destination,
        PixelRect {
            x: 1,
            y: 0,
            width: 4,
            height: 2,
        }
    );
    assert_eq!(
        placement_geometry(&pane, 7).sizing,
        PlacementSizing::Natural
    );
    assert_eq!(placement_geometry(&pane, 7).columns, Some(2));
    assert_eq!(placement_geometry(&pane, 7).rows, Some(2));
}

#[test]
fn kitty_terminal_pixels_only_infer_from_an_exact_cell_grid() {
    let mut pane = Pane::spawn("/bin/sh", 6, 20).unwrap();
    let terminal = Winsize {
        ws_row: 6,
        ws_col: 20,
        ws_xpixel: 40,
        ws_ypixel: 6,
    };
    let image = format!(
        "\x1b_Ga=T,f=24,s=4,v=2,i=7,p=1;{}\x1b\\",
        STANDARD.encode([0; 24])
    );
    pane.process_output_with_image_store_for_terminal(image.as_bytes(), &mut |_| {}, terminal);
    assert_eq!(placement_geometry(&pane, 1).columns, Some(2));
    assert_eq!(placement_geometry(&pane, 1).rows, Some(2));
    assert_eq!(pane.screen().cursor(), (2, 2));

    pane.process_output_with_image_store_for_terminal(
        b"\x1b_Ga=p,i=7,p=2\x1b\\",
        &mut |_| {},
        Winsize {
            ws_xpixel: 41,
            ..terminal
        },
    );
    assert_eq!(placement_geometry(&pane, 2).columns, None);
    assert_eq!(pane.screen().cursor(), (2, 2));

    pane.process_output_with_image_store_for_terminal(
        b"\x1b_Ga=p,i=7,p=3\x1b\\",
        &mut |_| {},
        terminal,
    );
    assert_eq!(placement_geometry(&pane, 3).columns, Some(2));
    assert_eq!(pane.screen().cursor(), (4, 4));
}

#[test]
fn kitty_source_crop_controls_inferred_extent_and_cursor_motion() {
    let mut pane = Pane::spawn("/bin/sh", 12, 30).unwrap();
    let cell = CellPixelSize::new(2, 1).unwrap();
    let image = format!(
        "\x1b_Ga=T,f=24,s=6,v=4,i=7,p=1,x=2,y=1,w=3,h=2;{}\x1b\\",
        STANDARD.encode([0; 72])
    );
    pane.process_output_with_image_store_sized(image.as_bytes(), &mut |_| {}, cell);
    assert_eq!(
        (
            placement_geometry(&pane, 1).columns,
            placement_geometry(&pane, 1).rows
        ),
        (Some(2), Some(2))
    );
    assert_eq!(placement_geometry(&pane, 1).source.left, 2);
    assert_eq!(placement_geometry(&pane, 1).source.width, Some(3));
    assert_eq!(pane.screen().cursor(), (2, 2));

    pane.process_output_with_image_store_sized(
        b"\x1b_Ga=p,i=7,p=2,x=2,y=1\x1b\\",
        &mut |_| {},
        cell,
    );
    assert_eq!(
        (
            placement_geometry(&pane, 2).columns,
            placement_geometry(&pane, 2).rows
        ),
        (Some(2), Some(3))
    );
    assert_eq!(pane.screen().cursor(), (5, 4));

    pane.process_output_with_image_store_sized(
        b"\x1b_Ga=p,i=7,p=3,x=5,y=3,w=9,h=9,C=1\x1b\\",
        &mut |_| {},
        cell,
    );
    assert_eq!(
        (
            placement_geometry(&pane, 3).columns,
            placement_geometry(&pane, 3).rows
        ),
        (Some(1), Some(1))
    );
    assert_eq!(pane.screen().cursor(), (5, 4));

    pane.process_output_with_image_store_sized(
        b"\x1b_Ga=p,i=7,p=4,x=6\x1b\\\x1b_Ga=p,i=7,p=5,x=bad\x1b\\",
        &mut |_| {},
        cell,
    );
    assert_eq!(placement_geometry(&pane, 4).source.left, 6);
    assert_eq!(placement_geometry(&pane, 4).columns, None);
    assert_eq!(pane.image_store().placements().count(), 4);
    assert_eq!(pane.screen().cursor(), (5, 4));

    pane.process_output_with_image_store_sized(
        b"\x1b_Ga=p,i=7,p=6,x=2,y=1,w=3,h=2,c=4,C=1\x1b\\",
        &mut |_| {},
        cell,
    );
    assert_eq!(placement_geometry(&pane, 6).columns, Some(4));
    assert_eq!(placement_geometry(&pane, 6).rows, Some(6));
    assert_eq!(pane.screen().cursor(), (5, 4));
}

#[test]
fn kitty_cell_pixel_offsets_validate_before_mutation_and_do_not_extend_cells() {
    let mut pane = Pane::spawn("/bin/sh", 8, 20).unwrap();
    let cell = CellPixelSize::new(2, 1).unwrap();
    let image = format!(
        "\x1b_Ga=T,f=24,s=4,v=2,i=7,p=1,X=1,Y=0,c=2,r=2,C=1;{}\x1b\\",
        STANDARD.encode([0; 24])
    );
    pane.process_output_with_image_store_sized(image.as_bytes(), &mut |_| {}, cell);
    assert_eq!(placement_geometry(&pane, 1).cell_offset.x, 1);
    assert_eq!(placement_geometry(&pane, 1).cell_offset.y, 0);
    assert_eq!(pane.screen().cursor(), (0, 0));

    pane.process_output_with_image_store_sized(
        b"\x1b_Ga=p,i=7,p=2,X=2,c=1,r=1\x1b\\\x1b_Ga=p,i=7,p=3,Y=1,c=1,r=1\x1b\\\x1b_Ga=p,i=7,p=4,X=bad\x1b\\",
        &mut |_| {},
        cell,
    );
    assert_eq!(pane.image_store().placements().count(), 1);
    assert_eq!(pane.screen().cursor(), (0, 0));

    let invalid_replacement = format!(
        "\x1b_Ga=T,f=24,s=4,v=2,i=7,p=4,X=2;{}\x1b\\",
        STANDARD.encode([1; 24])
    );
    pane.process_output_with_image_store_sized(invalid_replacement.as_bytes(), &mut |_| {}, cell);
    assert_eq!(pane.image_store().get(7).unwrap().data, [0; 24]);
    assert_eq!(pane.image_store().placements().count(), 1);

    pane.process_output_with_image_store_sized(
        b"\x1b_Ga=p,i=7,p=5,X=1,Y=0\x1b\\",
        &mut |_| {},
        cell,
    );
    assert_eq!(placement_geometry(&pane, 5).cell_offset.x, 1);
    assert_eq!(placement_geometry(&pane, 5).columns, Some(2));
    assert_eq!(placement_geometry(&pane, 5).rows, Some(2));
    assert_eq!(pane.screen().cursor(), (2, 2));

    pane.process_output_with_image_store(b"\x1b_Ga=p,i=7,p=6,X=99,Y=99,C=1\x1b\\", &mut |_| {});
    assert_eq!(placement_geometry(&pane, 6).cell_offset.x, 99);
    assert_eq!(placement_geometry(&pane, 6).cell_offset.y, 99);
    assert_eq!(pane.screen().cursor(), (2, 2));
}

#[test]
fn kitty_sized_png_inference_recovers_after_invalid_image_replacement() {
    let mut pane = Pane::spawn("/bin/sh", 6, 20).unwrap();
    let cell = CellPixelSize::new(2, 1).unwrap();
    pane.process_output_with_image_store_sized(
        b"\x1b_Ga=T,f=100,i=7,p=1;QQ==\x1b\\",
        &mut |_| {},
        cell,
    );
    assert_eq!(placement_geometry(&pane, 1).rows, None);
    assert_eq!(pane.screen().cursor(), (0, 0));

    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, 3, 2);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0; 18]).unwrap();
    }
    let image = format!(
        "\x1b_Ga=T,f=100,i=7,p=2;{}\x1b\\",
        STANDARD.encode(png_bytes)
    );
    pane.process_output_with_image_store_sized(image.as_bytes(), &mut |_| {}, cell);
    assert_eq!(pane.image_store().placements().count(), 1);
    assert_eq!(
        (
            placement_geometry(&pane, 2).columns,
            placement_geometry(&pane, 2).rows
        ),
        (Some(2), Some(2))
    );
    assert_eq!(pane.screen().cursor(), (2, 2));
}

#[test]
fn kitty_screen_clear_only_removes_active_anchored_placements() {
    let mut pane = Pane::spawn("/bin/sh", 6, 40).unwrap();
    pane.process_output_with_image_store(b"\x1b_Ga=T,f=100,i=7,p=1;QQ==\x1b\\", &mut |_| {});
    for erase in [
        b"\x1b[J".as_slice(),
        b"\x1b[1J",
        b"\x1b[2K",
        b"\x1b[3J",
        b"\x1b[!p",
    ] {
        pane.process_output_with_image_store(erase, &mut |_| {});
        assert_eq!(pane.image_store().placements().count(), 1);
    }
    pane.process_output_with_image_store(b"\x1b[2", &mut |_| {});
    assert_eq!(pane.image_store().placements().count(), 1);
    pane.process_output_with_image_store(b"J\x1b_Ga=p,i=7,p=2\x1b\\", &mut |_| {});
    let placements: Vec<_> = pane.image_store().placements().copied().collect();
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].placement_id, Some(2));
    assert!(pane.image_store().get(7).is_some());
}

#[test]
fn kitty_alternate_screen_clears_do_not_remove_main_placements() {
    let mut pane = Pane::spawn("/bin/sh", 6, 40).unwrap();
    pane.process_output_with_image_store(b"\x1b_Ga=T,f=100,i=7,p=1;QQ==\x1b\\", &mut |_| {});
    pane.process_output_with_image_store(b"\x1b[?1049h\x1b_Ga=p,i=7,p=2\x1b\\", &mut |_| {});
    assert_eq!(pane.image_store().placements().count(), 2);
    pane.process_output_with_image_store(b"\x1b[?1049h", &mut |_| {});
    assert_eq!(pane.image_store().placements().count(), 2);
    pane.process_output_with_image_store(b"\x1b[2J", &mut |_| {});
    assert_eq!(pane.image_store().placements().count(), 1);
    assert_eq!(
        pane.image_store().placements().next().unwrap().placement_id,
        Some(1)
    );
    pane.process_output_with_image_store(b"\x1b_Ga=p,i=7,p=3\x1b\\\x1b[?1049l", &mut |_| {});
    assert_eq!(pane.image_store().placements().count(), 1);
    pane.process_output_with_image_store(b"\x1b[?47h\x1b_Ga=p,i=7,p=4\x1b\\\x1b[?47l", &mut |_| {});
    assert_eq!(pane.image_store().placements().count(), 2);
    pane.process_output_with_image_store(b"\x1b[?1049h", &mut |_| {});
    assert_eq!(pane.image_store().placements().count(), 1);
    pane.process_output_with_image_store(b"\x1b[?1049l", &mut |_| {});
    pane.process_output_with_image_store(b"\x1b[?1047h\x1b_Ga=p,i=7,p=5\x1b\\", &mut |_| {});
    assert_eq!(pane.image_store().placements().count(), 2);
    pane.process_output_with_image_store(b"\x1b[?1047l", &mut |_| {});
    assert_eq!(pane.image_store().placements().count(), 1);
    assert!(pane.image_store().get(7).is_some());
}

#[test]
fn kitty_visible_delete_scopes_current_screen_and_hard_delete_frees_orphans() {
    let mut pane = Pane::spawn("/bin/sh", 3, 3).unwrap();
    let cell = CellPixelSize::new(1, 1).unwrap();
    pane.process_output_with_image_store_sized(
        b"\x1b_Ga=T,f=32,s=1,v=1,i=7,p=1,C=1;AQIDBA==\x1b\\\x1b[?1049h\x1b_Ga=p,i=7,p=2,C=1\x1b\\",
        &mut |_| {},
        cell,
    );
    assert_eq!(pane.image_store().placements().count(), 2);
    pane.process_output_with_image_store_sized(b"\x1b_Ga=d\x1b\\", &mut |_| {}, cell);
    assert_eq!(pane.image_store().placements().count(), 1);
    assert_eq!(
        pane.image_store().placements().next().unwrap().placement_id,
        Some(1)
    );
    assert!(pane.image_store().get(7).is_some());

    pane.process_output_with_image_store_sized(
        b"\x1b_Ga=p,i=7,p=3,C=1\x1b\\\x1b_Ga=d,d=A\x1b\\",
        &mut |_| {},
        cell,
    );
    assert_eq!(pane.image_store().placements().count(), 1);
    assert!(pane.image_store().get(7).is_some());
    pane.process_output_with_image_store_sized(
        b"\x1b[?1049l\x1b_Ga=d,d=A\x1b\\",
        &mut |_| {},
        cell,
    );
    assert_eq!(pane.image_store().placements().count(), 0);
    assert!(pane.image_store().get(7).is_none());
}

#[test]
fn kitty_visible_delete_keeps_scrollback_but_removes_partial_overlap() {
    let mut pane = Pane::spawn("/bin/sh", 3, 3).unwrap();
    let cell = CellPixelSize::new(1, 1).unwrap();
    pane.process_output_with_image_store_sized(
        b"\x1b_Ga=T,f=32,s=1,v=1,i=7,p=1,C=1;AQIDBA==\x1b\\\x1b_Ga=T,f=32,s=1,v=2,i=8,p=1,C=1;AQIDBAUGBwg=\x1b\\\x1b[3;1H\n",
        &mut |_| {},
        cell,
    );
    assert_eq!(placement_geometry(&pane, 1).row_offset, -1);
    assert_eq!(pane.image_store().placements().count(), 2);
    pane.process_output_with_image_store_sized(b"\x1b_Ga=d,d=a\x1b\\", &mut |_| {}, cell);
    let placements: Vec<_> = pane.image_store().placements().copied().collect();
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].image_id, 7);
    assert!(pane.image_store().get(8).is_some());

    let revision = pane.image_store().revision();
    pane.process_output_with_image_store_sized(
        b"\x1b_Ga=d,d=A,i=7\x1b\\\x1b_Ga=d,d=a,p=1\x1b\\",
        &mut |_| {},
        cell,
    );
    assert_eq!(pane.image_store().revision(), revision);
    pane.process_output_with_image_store_sized(b"\x1b_Ga=d,d=A\x1b\\", &mut |_| {}, cell);
    assert!(pane.image_store().get(7).is_some());
    assert_eq!(pane.image_store().placements().count(), 1);
}

#[test]
fn kitty_ris_clears_both_screens_but_later_put_in_same_chunk_survives() {
    let mut pane = Pane::spawn("/bin/sh", 6, 40).unwrap();
    pane.process_output_with_image_store(b"\x1b_Ga=T,f=100,i=7,p=1;QQ==\x1b\\", &mut |_| {});
    pane.process_output_with_image_store(b"\x1b[?47h\x1b_Ga=p,i=7,p=2\x1b\\", &mut |_| {});
    assert_eq!(pane.image_store().placements().count(), 2);
    pane.process_output_with_image_store(b"\x1bc\x1b_Ga=p,i=7,p=3\x1b\\", &mut |_| {});
    let placements: Vec<_> = pane.image_store().placements().copied().collect();
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].placement_id, Some(3));
    assert!(!placements[0].geometry.unwrap().anchor.alternate);
    assert!(pane.image_store().get(7).is_some());
}

#[test]
fn kitty_placements_follow_full_screen_scrolling_and_keep_main_scrollback_refs() {
    let mut pane = Pane::spawn("/bin/sh", 3, 20).unwrap();
    pane.process_output_with_image_store(
        b"\x1b[1;1H\x1b_Ga=T,f=100,i=7,p=1,r=1;QQ==\x1b\\\x1b[2;1H\x1b_Ga=p,i=7,p=2,r=1\x1b\\",
        &mut |_| {},
    );
    pane.process_output_with_image_store(b"\x1b[3;1H\n", &mut |_| {});
    let placements: Vec<_> = pane.image_store().placements().copied().collect();
    assert_eq!(placements.len(), 2);
    assert_eq!(placements[0].geometry.unwrap().row_offset, -1);
    assert_eq!(placements[1].geometry.unwrap().row_offset, -1);
    assert!(pane.screen().history_len() >= 1);
    pane.process_output_with_image_store(b"\x1b[3;1H\n\n", &mut |_| {});
    assert_eq!(placement_geometry(&pane, 1).row_offset, -3);
    assert_eq!(placement_geometry(&pane, 2).row_offset, -3);

    pane.process_output_with_image_store(
        b"\x1b[?1049h\x1b[1;1H\x1b_Ga=p,i=7,p=3,r=1\x1b\\\x1b[2;1H\x1b_Ga=p,i=7,p=4,r=1\x1b\\\x1b[3;1H\n",
        &mut |_| {},
    );
    let placements: Vec<_> = pane.image_store().placements().copied().collect();
    assert_eq!(placements.len(), 3);
    assert!(placements.iter().all(|p| p.placement_id != Some(3)));
    assert_eq!(
        placements
            .iter()
            .find(|p| p.placement_id == Some(4))
            .unwrap()
            .geometry
            .unwrap()
            .row_offset,
        -1
    );
}

#[test]
fn kitty_margin_scroll_clips_known_height_and_leaves_unknown_extent_alone() {
    let mut pane = Pane::spawn("/bin/sh", 5, 20).unwrap();
    pane.process_output_with_image_store(
        b"\x1b_Ga=t,f=100,i=7;QQ==\x1b\\\x1b[1;1H\x1b_Ga=p,i=7,p=1,r=2\x1b\\\x1b[2;1H\x1b_Ga=p,i=7,p=2,r=2\x1b\\\x1b_Ga=p,i=7,p=3\x1b\\",
        &mut |_| {},
    );
    pane.process_output_with_image_store(b"\x1b[2;4r\x1b[4;1H\n", &mut |_| {});
    assert_eq!(placement_geometry(&pane, 1).row_offset, 0); // Crosses the top margin.
    assert_eq!(placement_geometry(&pane, 2).row_offset, -1);
    assert_eq!(placement_geometry(&pane, 2).clip_top_rows, 1);
    assert_eq!(placement_geometry(&pane, 3).row_offset, 0); // Height cannot be inferred yet.
    pane.process_output_with_image_store(b"\x1b[4;1H\n", &mut |_| {});
    assert!(
        pane.image_store()
            .placements()
            .all(|p| p.placement_id != Some(2))
    );
    assert_eq!(placement_geometry(&pane, 1).row_offset, 0);
    assert_eq!(placement_geometry(&pane, 3).row_offset, 0);
    pane.process_output_with_image_store(b"\x1b[1;3r\x1b[3;1H\n", &mut |_| {});
    assert_eq!(placement_geometry(&pane, 3).row_offset, 0);
}

#[test]
fn kitty_margin_scroll_clip_is_applied_to_visible_pixels() {
    let mut pane = Pane::spawn("/bin/sh", 5, 20).unwrap();
    let cell = CellPixelSize::new(1, 1).unwrap();
    let image = format!(
        "\x1b_Ga=t,f=32,s=1,v=2,i=7;{}\x1b\\",
        STANDARD.encode([10, 0, 0, 255, 20, 0, 0, 255])
    );
    pane.process_output_with_image_store_sized(image.as_bytes(), &mut |_| {}, cell);
    pane.process_output_with_image_store_sized(
        b"\x1b[2;1H\x1b_Ga=p,i=7,p=1,c=1,r=2,C=1\x1b\\",
        &mut |_| {},
        cell,
    );
    pane.process_output_with_image_store_sized(b"\x1b[2;4r\x1b[4;1H\n", &mut |_| {}, cell);
    let geometry = placement_geometry(&pane, 1);
    assert_eq!(geometry.row_offset, -1);
    assert_eq!(geometry.clip_top_rows, 1);
    let decoded = pane.image_store().get(7).unwrap().decode_rgba().unwrap();
    let layout = geometry
        .pixel_layout(decoded.width, decoded.height, cell)
        .unwrap();
    let pixels = decoded.resample_placement(layout).unwrap();
    let clipped = pixels
        .clip_to_viewport_with_scroll_clip(
            geometry,
            cell,
            PixelSize {
                width: 20,
                height: 5,
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        clipped.destination,
        PixelRect {
            x: 0,
            y: 1,
            width: 1,
            height: 1
        }
    );
    assert_eq!(clipped.pixels, [20, 0, 0, 255]);
    let snapshot = pane.compose_image_snapshot(cell).unwrap();
    assert_eq!(&snapshot.pixels[20 * 4..20 * 4 + 4], &[20, 0, 0, 255]);
    assert!(snapshot.pixels[..20 * 4].iter().all(|&byte| byte == 0));
}

#[test]
fn kitty_clipped_placements_compose_in_image_id_order() {
    let mut pane = Pane::spawn("/bin/sh", 2, 2).unwrap();
    let cell = CellPixelSize::new(1, 1).unwrap();
    for (id, rgba) in [(8, [0, 0, 255, 128]), (7, [255, 0, 0, 128])] {
        let image = format!(
            "\x1b_Ga=T,f=32,s=1,v=1,i={id},p=1,c=1,r=1,z=0,C=1;{}\x1b\\",
            STANDARD.encode(rgba)
        );
        pane.process_output_with_image_store_sized(image.as_bytes(), &mut |_| {}, cell);
    }
    let mut clipped = Vec::new();
    let mut keys = Vec::new();
    for placement in pane.image_store().placements() {
        let geometry = placement.geometry.unwrap();
        let image = pane
            .image_store()
            .get(placement.image_id)
            .unwrap()
            .decode_rgba()
            .unwrap();
        let layout = geometry
            .pixel_layout(image.width, image.height, cell)
            .unwrap();
        let content = image.resample_placement(layout).unwrap();
        clipped.push(
            content
                .clip_to_viewport_with_scroll_clip(
                    geometry,
                    cell,
                    PixelSize {
                        width: 2,
                        height: 2,
                    },
                )
                .unwrap()
                .unwrap(),
        );
        keys.push((placement.image_id, geometry.z_index));
    }
    let layers: Vec<_> = clipped
        .iter()
        .zip(keys)
        .map(|(placement, (image_id, z_index))| ImageLayer {
            image_id,
            z_index,
            placement,
        })
        .collect();
    let canvas = compose_image_layers(
        PixelSize {
            width: 2,
            height: 2,
        },
        &layers,
    )
    .unwrap();
    assert_eq!(&canvas.pixels[..4], &[85, 0, 170, 192]);
    assert!(canvas.pixels[4..].iter().all(|&byte| byte == 0));
    assert_eq!(pane.compose_image_snapshot(cell).unwrap(), canvas);
    let planes = pane.compose_image_planes(cell).unwrap();
    assert_eq!(planes.above_text, Some(canvas));
    assert_eq!(planes.behind_text, None);
    assert_eq!(planes.behind_background, None);
}

#[test]
fn kitty_image_planes_keep_text_and_background_z_boundaries() {
    let mut pane = Pane::spawn("/bin/sh", 1, 6).unwrap();
    let cell = CellPixelSize::new(1, 1).unwrap();
    let empty = pane.compose_image_planes(cell).unwrap();
    assert_eq!(empty.behind_background, None);
    assert_eq!(empty.behind_text, None);
    assert_eq!(empty.above_text, None);
    let layers = [
        i32::MIN,
        BACKGROUND_Z_BOUNDARY - 1,
        BACKGROUND_Z_BOUNDARY,
        -1,
        0,
        i32::MAX,
    ];
    for (column, z) in layers.into_iter().enumerate() {
        let id = column + 1;
        let image = format!(
            "\x1b[1;{}H\x1b_Ga=T,f=32,s=1,v=1,i={id},p=1,c=1,r=1,z={z},C=1;{}\x1b\\",
            column + 1,
            STANDARD.encode([id as u8, 0, 0, 255])
        );
        pane.process_output_with_image_store_sized(image.as_bytes(), &mut |_| {}, cell);
    }
    let planes = pane.compose_image_planes(cell).unwrap();
    for (band, expected) in [
        (ImageBand::BehindBackground, &planes.behind_background),
        (ImageBand::BehindText, &planes.behind_text),
        (ImageBand::AboveText, &planes.above_text),
    ] {
        assert_eq!(
            pane.compose_image_band(cell, band).unwrap().as_ref(),
            expected.as_ref()
        );
    }
    assert_eq!(ImageBand::BehindBackground.output_z(), i32::MIN);
    assert_eq!(ImageBand::BehindText.output_z(), -1);
    assert_eq!(ImageBand::AboveText.output_z(), 0);
    for (plane, occupied) in [
        (planes.behind_background.as_ref().unwrap(), 0..2),
        (planes.behind_text.as_ref().unwrap(), 2..4),
        (planes.above_text.as_ref().unwrap(), 4..6),
    ] {
        for column in 0..6 {
            let pixel = &plane.pixels[column * 4..column * 4 + 4];
            if occupied.contains(&column) {
                assert_eq!(pixel, &[(column + 1) as u8, 0, 0, 255]);
            } else {
                assert_eq!(pixel, &[0, 0, 0, 0]);
            }
        }
    }
    pane.process_output_with_image_store_sized(b"\x1b[?1047h", &mut |_| {}, cell);
    let alternate = pane.compose_image_planes(cell).unwrap();
    assert_eq!(alternate.behind_background, None);
    assert_eq!(alternate.behind_text, None);
    assert_eq!(alternate.above_text, None);
}

#[test]
fn kitty_image_planes_reject_three_full_size_canvases_before_allocating_them() {
    let mut pane = Pane::spawn("/bin/sh", 1, 2).unwrap();
    let cell = CellPixelSize::new(2048, 2048).unwrap();
    for (id, z) in [(1, i32::MIN), (2, -1), (3, 0)] {
        let image = format!(
            "\x1b_Ga=T,f=32,s=1,v=1,i={id},p=1,c=1,r=1,z={z},C=1;{}\x1b\\",
            STANDARD.encode([id as u8, 0, 0, 255])
        );
        pane.process_output_with_image_store_sized(image.as_bytes(), &mut |_| {}, cell);
    }
    assert_eq!(
        pane.compose_image_planes(cell),
        Err(SnapshotError::OutputLimit)
    );
}

#[test]
fn kitty_image_snapshot_selects_current_screen_without_mutating_references() {
    let mut pane = Pane::spawn("/bin/sh", 2, 2).unwrap();
    let cell = CellPixelSize::new(1, 1).unwrap();
    let image = format!(
        "\x1b_Ga=T,f=32,s=1,v=1,i=7,p=1,c=1,r=1,C=1;{}\x1b\\",
        STANDARD.encode([255, 0, 0, 255])
    );
    pane.process_output_with_image_store_sized(image.as_bytes(), &mut |_| {}, cell);
    let main = pane.compose_image_snapshot(cell).unwrap();
    assert_eq!(&main.pixels[..4], &[255, 0, 0, 255]);

    pane.process_output_with_image_store_sized(b"\x1b[?1047h", &mut |_| {}, cell);
    let alternate = pane.compose_image_snapshot(cell).unwrap();
    assert!(alternate.pixels.iter().all(|&byte| byte == 0));
    pane.process_output_with_image_store_sized(b"\x1b[?1047l", &mut |_| {}, cell);
    assert_eq!(pane.compose_image_snapshot(cell).unwrap(), main);
    assert_eq!(pane.image_store().placements().count(), 1);
}

#[test]
fn kitty_image_snapshot_resolves_unsized_natural_placement_on_demand() {
    let mut pane = Pane::spawn("/bin/sh", 2, 2).unwrap();
    let image = format!(
        "\x1b_Ga=T,f=32,s=1,v=1,i=7,p=1,C=1;{}\x1b\\",
        STANDARD.encode([3, 4, 5, 255])
    );
    pane.process_output_with_image_store(image.as_bytes(), &mut |_| {});
    let geometry = placement_geometry(&pane, 1);
    assert_eq!(geometry.columns, None);
    assert_eq!(geometry.rows, None);
    let snapshot = pane
        .compose_image_snapshot(CellPixelSize::new(2, 2).unwrap())
        .unwrap();
    assert_eq!(&snapshot.pixels[..4], &[3, 4, 5, 255]);
    assert!(snapshot.pixels[4..].iter().all(|&byte| byte == 0));
}

#[test]
fn kitty_image_snapshot_reports_invalid_data_and_oversized_canvas() {
    let mut pane = Pane::spawn("/bin/sh", 2, 2).unwrap();
    pane.process_output_with_image_store(
        b"\x1b_Ga=T,f=100,i=7,p=1,c=1,r=1,C=1;QQ==\x1b\\",
        &mut |_| {},
    );
    let cell = CellPixelSize::new(1, 1).unwrap();
    assert!(matches!(
        pane.compose_image_snapshot(cell),
        Err(SnapshotError::Decode(_))
    ));
    assert!(matches!(
        pane.compose_image_planes(cell),
        Err(SnapshotError::Decode(_))
    ));
    assert_eq!(pane.image_store().placements().count(), 1);

    let huge_cell = CellPixelSize::new(4096, 4096).unwrap();
    assert_eq!(
        pane.compose_image_snapshot(huge_cell),
        Err(SnapshotError::OutputLimit)
    );
    assert_eq!(
        compose_store_snapshot(
            pane.image_store(),
            false,
            PixelSize {
                width: u32::MAX,
                height: u32::MAX,
            },
            cell,
        ),
        Err(SnapshotError::OutputLimit)
    );
}

#[test]
fn kitty_reverse_index_and_large_event_batches_keep_positions_bounded() {
    let mut pane = Pane::spawn("/bin/sh", 3, 20).unwrap();
    pane.process_output_with_image_store(
        b"\x1b[2;1H\x1b_Ga=T,f=100,i=7,p=1,r=2;QQ==\x1b\\\x1b[1;1H\x1bM",
        &mut |_| {},
    );
    let geometry = pane
        .image_store()
        .placements()
        .next()
        .unwrap()
        .geometry
        .unwrap();
    assert_eq!(geometry.row_offset, 1);
    assert_eq!(geometry.clip_bottom_rows, 1);

    let mut input = Vec::new();
    for _ in 0..129 {
        input.extend_from_slice(b"\x1b[S\x1b[T");
    }
    pane.process_output_with_image_store(&input, &mut |_| {});
    assert_eq!(pane.image_store().placements().count(), 0);
    assert!(pane.image_store().get(7).is_some());
}

#[test]
fn real_windows_keep_processes_and_terminal_state_isolated() {
    assert!(Pane::spawn("/definitely/missing/rustmux-shell", 24, 80).is_err());
    assert!(Pane::spawn("/bin/sh", 0, 80).is_err());
    assert!(Pane::spawn("/bin/sh", 257, 256).is_err());
    let mut windows = Windows::default();
    let a = windows
        .create("a".into(), Pane::spawn("/bin/sh", 24, 80).unwrap())
        .unwrap();
    let b = windows
        .create("b".into(), Pane::spawn("/bin/sh", 24, 80).unwrap())
        .unwrap();
    for id in [a, b] {
        let pane = windows.get_mut(id).unwrap().content_mut();
        let flags = fcntl(pane.shell().master_fd().unwrap(), FcntlArg::F_GETFL).unwrap();
        assert!(OFlag::from_bits_truncate(flags).contains(OFlag::O_NONBLOCK));
        // Wait for the initial prompt before commands: shell startup may flush input.
        until(pane, |p| !text(p).trim().is_empty());
    }
    let a_pid = windows.get(a).unwrap().content().shell().id();
    let b_pid = windows.get(b).unwrap().content().shell().id();
    assert_ne!(a_pid, b_pid);
    windows.select(a).unwrap();
    for (id, suffix) in [(a, "A"), (b, "B")] {
        let pane = windows.get_mut(id).unwrap().content_mut();
        pane.shell_mut()
            .write_all(
                format!("stty -echo; printf '\\033[2J\\033[H%s%s\\n' READY_ {suffix}\n").as_bytes(),
            )
            .unwrap();
        until(pane, |p| text(p).starts_with(&format!("READY_{suffix}")));
    }
    assert_eq!(windows.active().unwrap().id(), a);
    for _ in 0..4 {
        windows.select_next();
    }
    assert_eq!(windows.get(a).unwrap().content().shell().id(), a_pid);
    assert_eq!(windows.get(b).unwrap().content().shell().id(), b_pid);
    // Each parser retains partial input and produces replies from its own grid.
    let pane = windows.get_mut(a).unwrap().content_mut();
    pane.process_output(b"\x1b[3;4H\x1b[?2004h\x1b[6", &mut |_| {});
    let mut replies = Vec::new();
    pane.process_output(b"n", &mut |reply| replies.extend_from_slice(reply));
    assert_eq!(replies, b"\x1b[3;4R");
    assert!(pane.screen().bracketed_paste());
    assert!(!windows.get(b).unwrap().content().screen().bracketed_paste());
    let removed = windows.close(a).unwrap();
    assert!(
        windows
            .get_mut(b)
            .unwrap()
            .content_mut()
            .shell_mut()
            .try_wait()
            .unwrap()
            .is_none()
    );
    drop(removed);
    assert_eq!(
        waitpid(Pid::from_raw(a_pid as i32), Some(WaitPidFlag::WNOHANG)),
        Err(Errno::ECHILD)
    );
    let pane = windows.active_mut().unwrap().content_mut();
    pane.shell_mut()
        .write_all(b"printf '\\033[2J\\033[H%s%s\\n' STILL_ ALIVE\n")
        .unwrap();
    until(pane, |p| text(p).starts_with("STILL_ALIVE"));
    assert_eq!(pane.shell().id(), b_pid);
    pane.shell_mut().terminate().unwrap();
    prepared_resizes_preserve_state_and_update_the_real_pty();
    layout_sizes_reach_real_children_and_survive_zoom_and_close();
}

fn prepared_resizes_preserve_state_and_update_the_real_pty() {
    let mut pane = Pane::spawn("/bin/sh", 12, 40).unwrap();
    until(&mut pane, |p| !text(p).trim().is_empty());
    pane.process_output(b"\x1b[2J\x1b[Hkept\x1b[?2026h", &mut |_| {});
    let before = pane.screen().clone();
    assert!(pane.prepare_resize(0, 40).is_err());
    assert!(pane.prepare_resize(257, 256).is_err());
    drop(pane.prepare_resize(8, 30).unwrap());
    assert_eq!(pane.screen(), &before);
    pane.shell_mut()
        .write_all(b"stty -echo; printf '\\033[2J\\033[HOLD:'; stty size\n")
        .unwrap();
    until(&mut pane, |p| text(p).contains("OLD:12 40"));
    // An incomplete parser sequence must survive screen preparation and commit.
    pane.process_output(b"\x1b[2J\x1b[Hkept\x1b[?2026h\xe4\xb8", &mut |_| {});
    pane.prepare_resize(8, 30).unwrap().commit().unwrap();
    assert_eq!(pane.screen().dimensions(), (8, 30));
    assert!(!pane.screen().synchronized_output());
    pane.process_output(b"\xad", &mut |_| {});
    assert!(text(&pane).starts_with("kept中"));
    pane.shell_mut()
        .write_all(b"printf '\\033[2J\\033[HNEW:'; stty size\n")
        .unwrap();
    until(&mut pane, |p| text(p).contains("NEW:8 30"));
    pane.process_output(b"\x1b[?2026h", &mut |_| {});
    pane.prepare_resize(8, 30).unwrap().commit().unwrap();
    assert!(!pane.screen().synchronized_output());
    pane.process_output(b"\x1b[?2026h", &mut |_| {});
    pane.shell_mut().terminate().unwrap();
    let before = pane.screen().clone();
    assert!(pane.prepare_resize(6, 20).unwrap().commit().is_err());
    assert_eq!(pane.screen(), &before); // Closed master must not commit prepared cells.
    pane.finish_output();
    pane.prepare_resize(6, 20).unwrap().commit().unwrap(); // Known EOF skips ioctl.
    assert_eq!(pane.screen().dimensions(), (6, 20));
}

fn expect_size(pane: &mut Pane, marker: &str, rows: u16, columns: u16) {
    pane.shell_mut()
        .write_all(format!("stty -echo; printf '\\033[2J\\033[H{marker}:'; stty size\n").as_bytes())
        .unwrap();
    until(pane, |p| {
        text(p).contains(&format!("{marker}:{rows} {columns}"))
    });
    assert_eq!(
        pane.screen().dimensions(),
        (usize::from(rows), usize::from(columns))
    );
}

fn layout_sizes_reach_real_children_and_survive_zoom_and_close() {
    let mut panes = PaneSet::new(12, 41, Pane::spawn("/bin/sh", 10, 39).unwrap()).unwrap();
    let first = panes.layout().active();
    until(panes.active_mut(), |p| !text(p).trim().is_empty());
    let first_pid = panes.active().shell().id();
    let second = panes
        .split_with(SplitAxis::Columns, |_, rect| {
            Pane::spawn("/bin/sh", rect.rows, rect.columns)
        })
        .unwrap();
    until(panes.active_mut(), |p| !text(p).trim().is_empty());
    let second_pid = panes.active().shell().id();
    panes.synchronize_sizes().unwrap();
    expect_size(panes.get_mut(first).unwrap(), "SPLIT_A", 10, 18);
    expect_size(panes.get_mut(second).unwrap(), "SPLIT_B", 10, 19);
    panes
        .get_mut(first)
        .unwrap()
        .process_output(b"\x1b[?2026h", &mut |_| {});
    panes.select(first).unwrap();
    panes.synchronize_sizes().unwrap();
    assert!(panes.active().screen().synchronized_output()); // Focus alone isn't a resize.
    panes.toggle_zoom();
    panes.synchronize_sizes().unwrap();
    expect_size(panes.get_mut(first).unwrap(), "ZOOM_A", 10, 39);
    expect_size(panes.get_mut(second).unwrap(), "HIDDEN_B", 10, 19);
    panes.select(second).unwrap();
    panes.synchronize_sizes().unwrap();
    expect_size(panes.get_mut(first).unwrap(), "HIDDEN_A", 10, 18);
    expect_size(panes.get_mut(second).unwrap(), "ZOOM_B", 10, 39);
    panes.resize(9, 31).unwrap();
    panes.synchronize_sizes().unwrap();
    expect_size(panes.get_mut(first).unwrap(), "RESIZE_A", 7, 13);
    expect_size(panes.get_mut(second).unwrap(), "RESIZE_B", 7, 29);
    panes.toggle_zoom();
    panes.synchronize_sizes().unwrap();
    expect_size(panes.get_mut(second).unwrap(), "UNZOOM_B", 7, 14);
    // The geometry-only API permits this size; process synchronization rejects it
    // before altering any owned screen or terminal.
    panes.resize(257, 256).unwrap();
    assert!(panes.synchronize_sizes().is_err());
    assert_eq!(panes.get(first).unwrap().screen().dimensions(), (7, 13));
    assert_eq!(panes.get(second).unwrap().screen().dimensions(), (7, 14));
    panes.resize(9, 31).unwrap();
    assert_eq!(panes.get(first).unwrap().shell().id(), first_pid);
    assert_eq!(panes.get(second).unwrap().shell().id(), second_pid);
    let removed = panes.close(first).unwrap();
    panes.synchronize_sizes().unwrap();
    expect_size(panes.active_mut(), "SURVIVOR", 7, 29);
    assert_eq!(panes.active().shell().id(), second_pid);
    drop(removed);
    assert_eq!(
        waitpid(Pid::from_raw(first_pid as i32), Some(WaitPidFlag::WNOHANG)),
        Err(Errno::ECHILD)
    );
    // Exited panes still get model geometry without attempting to resize a closed PTY.
    panes.active_mut().shell_mut().terminate().unwrap();
    panes.resize(6, 21).unwrap();
    panes.synchronize_sizes().unwrap();
    assert_eq!(panes.active().screen().dimensions(), (4, 19));
}
