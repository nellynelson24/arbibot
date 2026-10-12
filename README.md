# federbot

Arbitraż spot OKX EEA ↔ Bybit EU. Rust.

## Budowa

```bash
cargo build --release
```

## Użycie

```bash
./target/release/scanner run
./target/release/bot dry-run --pair STRK/USDC --capital 200 --size 20
./target/release/bot live --pair STRK/USDC --size 20 --min-bps 35 --telegram
./target/release/bot report
```

Klucze API w pęku kluczy macOS (`federbot-<giełda>-read`, `federbot-<giełda>-trade`), bez prawa wypłat.

## Struktura

- `bins/bot` — bot
- `bins/scanner` — skaner okazji
- `bins/latency` — pomiar opóźnień
- `bins/pair-scan` — przegląd par
- `crates/account` — API kont
- `crates/venues` — giełdy i prowizje
