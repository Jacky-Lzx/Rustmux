//! Manual comparison: run the same example on both revisions in release mode.
use rustmux::{parser::Parser, render::Renderer, screen::Screen};
use std::{hint::black_box, time::Instant};
fn base(offset: usize) -> Screen {
    let mut screen = Screen::new(24, 120).unwrap();
    for row in 0..24 {
        let text = char::from(b'a' + ((row + offset) % 26) as u8)
            .to_string()
            .repeat(120);
        Parser::new().advance(&mut screen, format!("\x1b[{};1H{text}", row + 1).as_bytes());
    }
    screen
}
fn main() {
    let before = base(0);
    let mut single = before.clone();
    Parser::new().advance(&mut single, b"\x1b[12;60HX");
    let mut status = before.clone();
    Parser::new().advance(&mut status, b"\x1b[24;5HNORMAL\x1b[24;100H12:34");
    let mut scattered = before.clone();
    for col in (1..=120).step_by(4) {
        Parser::new().advance(&mut scattered, format!("\x1b[12;{col}HX").as_bytes());
    }
    let scrolling = base(1);
    for (name, after) in [
        ("single", single),
        ("status", status),
        ("scattered", scattered),
        ("scroll", scrolling),
    ] {
        let mut results = Vec::new();
        let mut output_bytes = 0;
        for _ in 0..5 {
            let mut renderer = Renderer::default();
            let mut bytes = Vec::new();
            renderer.render(&before, &mut bytes).unwrap();
            let start = Instant::now();
            let mut total = 0;
            for n in 0..4000 {
                bytes.clear();
                renderer
                    .render(
                        black_box(if n % 2 == 0 { &after } else { &before }),
                        &mut bytes,
                    )
                    .unwrap();
                total += black_box(bytes.len());
            }
            results.push(start.elapsed().as_nanos() / 4000);
            output_bytes = total / 4000;
        }
        results.sort();
        println!(
            "{name}: {output_bytes} bytes/frame, {} ns/frame (median of 5)",
            results[2]
        );
    }
}
