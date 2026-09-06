# system-fonts

System font discovery and locale-based preset selection for native platforms.
On wasm, font discovery is not supported and the `find_*` functions return empty results.

## Installation

```toml
[dependencies]
system-fonts = "0.1.2"
```

## Usage

```rust,no_run
use system_fonts::{find_for_system_locale, FontStyle};

let (_locale, region, fonts) = find_for_system_locale(FontStyle::Sans);
println!("region={region:?}, fonts={}", fonts.len());
```

```rust
use system_fonts::{region_from_locale, FontRegion};

assert_eq!(region_from_locale("ko_KR.UTF-8"), FontRegion::Korean);
assert_eq!(region_from_locale("zh-Hant-TW"), FontRegion::TraditionalChinese);
```

## License

MIT OR Apache-2.0
