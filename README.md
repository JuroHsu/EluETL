# Elu ETL

Excel ↔ Database 高效能跨平台 ETL 桌面工具（Windows / macOS / Linux）。

- **技術棧**：Rust + Tauri 2 + Angular + TailwindCSS
- **資料庫**：SQL Server（tiberius）、PostgreSQL / MySQL / SQLite（sqlx）
- **開發計畫**：見 [docs/development-plan.md](docs/development-plan.md)

## 開發環境

需求：Rust stable、Node.js 22+。

```bash
npm install        # 前端依賴
npm run tauri dev  # 開發模式（熱重載）
```

## 品質檢查（CI 同步執行）

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## 專案結構

- `src-tauri/` — Rust 後端：DB 驅動抽象（`db/`）、ETL 引擎（`etl/`，開發中）、機密管理（`security/`）
- `src/` — Angular 前端：連線管理、欄位對應、ETL 執行三大頁面
- `docs/` — 開發計畫書與設計文件

## 安全政策摘要

- 密碼不落地設定檔；Debug / 日誌一律遮罩（`SecretString`）
- 全程 TLS（rustls）；MSSQL 信任自簽憑證需明確 opt-in 並記入審計日誌
- 供應鏈：cargo-deny 授權白名單 + 漏洞掃描（見 `src-tauri/deny.toml`）
