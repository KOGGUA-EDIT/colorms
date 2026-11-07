use colorms::color::{
    PickerColor, Rgb8, contrast_ratio, hsv_to_rgb, parse_hex, readable_on, rgb_to_hsv,
};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn benches(c: &mut Criterion) {
    c.bench_function("hsv_to_rgb", |b| {
        b.iter(|| hsv_to_rgb(black_box(211.0), black_box(0.71), black_box(0.87)))
    });

    c.bench_function("rgb_to_hsv", |b| {
        b.iter(|| rgb_to_hsv(black_box(Rgb8::new(184, 102, 223))))
    });

    c.bench_function("hex_roundtrip", |b| {
        let color = PickerColor::new(211.0, 0.71, 0.87);
        b.iter(|| {
            let hex = black_box(color.hex());
            PickerColor::from_hex(&hex).unwrap()
        })
    });

    c.bench_function("material_tones", |b| {
        let color = PickerColor::new(256.4, 0.512, 0.643);
        b.iter(|| {
            black_box(color.tonal_container());
            black_box(color.surface_tint());
            black_box(readable_on(color.rgb()));
        })
    });

    c.bench_function("contrast_ratio", |b| {
        b.iter(|| contrast_ratio(black_box(Rgb8::new(30, 30, 30)), black_box(Rgb8::WHITE)))
    });

    c.bench_function("parse_hex", |b| b.iter(|| parse_hex(black_box("#8C5ACF"))));
}

criterion_group!(benches_group, benches);
criterion_main!(benches_group);
