# Rust Cross-Platform ETL Tool
## Excel ↔ Database 高效能桌面應用程式 — 開發計畫書 v2.0

| 專案代號 | 技術棧 | 授權方式 | 目標平台 |
|---|---|---|---|
| ELU-ETL-001 | Rust + Tauri 2 + Angular | MIT / Apache 2.0 | Windows / macOS / Linux |

> **v2.0 變更摘要**（2026-06-12）：
> 1. **MSSQL 驅動修正** — sqlx 0.7 起已移除 MSSQL 支援，改採 `tiberius`（純 Rust TDS 驅動），並新增 DB 抽象層設計統一四種資料庫。
> 2. 新增 **ETL 執行語意**（交易策略、checkpoint 續跑、錯誤政策）正式設計。
> 3. 新增 **安全性需求**（機密管理、TLS、最小權限、供應鏈安全）章節。
> 4. 新增 **型別轉換矩陣** 與 **CSV / Big5 編碼** 處理規格。
> 5. 新增 **測試策略** 章節（單元 / 整合 / E2E / 效能）。
> 6. 時程由 8 週調整為 **10 週**；程式碼簽署憑證申請提前至 Phase 0。

---

## 1. 專案概覽 Project Overview

### 1.1 背景與目標

本工具旨在提供一套高效能的跨平台桌面應用程式，讓使用者可透過圖形介面完成 Excel ↔ SQL 資料庫的 ETL（Extract, Transform, Load）作業，無需安裝額外驅動或依賴外部服務。

### 1.2 核心特色

- **零驅動依賴**：純 Rust Managed Driver，不需 ODBC / 系統驅動
- **跨平台一致性**：Windows / macOS / Linux 相同程式碼、相同行為
- **高效能核心**：Tokio 非同步 + Rayon 多執行緒平行 ETL 處理
- **企業級可靠性**：交易語意明確、可中斷續跑、完整審計日誌
- **商業友善授權**：所有核心套件均為 MIT / Apache 2.0
- **原生應用體驗**：安裝包 < 10MB（不含 WebView runtime）

### 1.3 框架選型決策

在評估 egui、Slint、Tauri 2 三種 Rust 桌面框架後，最終選擇 **Tauri 2 + Angular**：

| 框架 | 授權 | 商業可用 | 記憶體 | UI 彈性 | 開發速度 | 決策 |
|---|---|---|---|---|---|---|
| **Tauri 2** | MIT/Apache | ✅ 免費 | ~30MB（核心） | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ✅ **選用** |
| egui | MIT/Apache | ✅ 免費 | ~15MB | ⭐⭐⭐ | ⭐⭐⭐ | ❌ Immediate Mode 耗電 |
| Slint | GPL/商業 | ⚠️ 需付費 | ~20MB | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ❌ 閉源需授權費 |

> **egui 排除原因**：Immediate Mode GUI 每幀重繪整個畫面，idle 狀態持續消耗 CPU/GPU，不適合 ETL 工具（大部分時間靜止等待）。
>
> **Slint 排除原因**：閉源商業使用需 ~$400/開發者/年授權費，增加商業化成本與法律風險。

---

## 2. 系統架構 System Architecture

### 2.1 整體架構層次

```
┌─────────────────────────────────────────┐
│      Presentation Layer                  │
│      Angular (LTS) + TailwindCSS         │  UI 元件、使用者互動、資料呈現
├─────────────────────────────────────────┤
│      Bridge Layer                        │
│      Tauri 2 IPC Commands + Channel      │  前後端溝通、進度事件、視窗管理
├─────────────────────────────────────────┤
│      Core Layer (Rust)                   │
│      Tokio + Rayon + sqlx + tiberius     │  ETL 邏輯、DB 連線、Excel 解析
└─────────────────────────────────────────┘
            ↕ 直接呼叫 OS API
      Windows / macOS / Linux
```

**IPC 設計原則（重要）**：大量資料**永不跨越 IPC 邊界**。
- 預覽：僅傳前 100 行至前端。
- 匯出：DB → xlsx 全程在 Rust 端以串流完成（sqlx cursor → rust_xlsxwriter constant-memory mode），前端僅接收進度事件。
- 進度：使用 Tauri 2 `ipc::Channel` 推送結構化進度事件，避免輪詢。

### 2.2 Rust 核心模組

#### 2.2.1 資料庫連線模組（`db/`）

> ⚠️ **驅動修正**：sqlx 自 0.7 起已**移除 MSSQL 支援**（0.6 時期僅為實驗性功能）。
> SQL Server 一律使用 `tiberius`（純 Rust TDS 驅動，MIT/Apache，支援 TDS Bulk Load）。

**雙驅動統一抽象**：以 trait 將兩套驅動整合為單一介面，上層（ETL 引擎、IPC commands）不感知底層差異：

```rust
// db/driver.rs
#[async_trait]
pub trait DbDriver: Send + Sync {
    async fn test_connection(&self) -> Result<(), EluEtlError>;
    async fn list_tables(&self) -> Result<Vec<TableInfo>, EluEtlError>;
    async fn get_columns(&self, table: &str) -> Result<Vec<ColumnInfo>, EluEtlError>;
    /// 串流查詢：回傳 row stream，供匯出 / 預覽使用，不一次性物化
    async fn query_stream(&self, sql: &str) -> Result<RowStream, EluEtlError>;
    /// 批次寫入：在指定交易內寫入一個 batch，回傳實際寫入行數
    async fn bulk_insert(&self, ctx: &mut WriteContext, batch: &RecordBatch)
        -> Result<u64, EluEtlError>;
}
```

| 資料庫 | 驅動 | 連線池 | 批次寫入機制 |
|---|---|---|---|
| SQL Server | tiberius（TDS, rustls TLS） | deadpool | TDS Bulk Load（`Client::bulk_insert`，等同 BCP） |
| PostgreSQL | sqlx 0.8 | sqlx 內建 | `COPY FROM STDIN`（binary protocol） |
| MySQL | sqlx 0.8 | sqlx 內建 | 多列 INSERT 批次（受 `max_allowed_packet` 約束自動分批） |
| SQLite | sqlx 0.8 | sqlx 內建 | 單一交易 + prepared statement；`journal_mode=WAL`、`synchronous=NORMAL` |

- 連線池管理：每組連線設定一個池，以 `ConnectionId` 為鍵快取於 `tauri::State<AppState>`，**禁止在 command 內臨時建池**
- 池參數：最大 10 連線、idle timeout 300s、acquire timeout 30s（逾時回報明確錯誤而非無限等待）
- 非同步查詢：全程 `async/await`，不阻塞 UI
- TLS：一律 rustls；MSSQL 預設 `encrypt=true`，信任自簽憑證（`trust_server_certificate`）必須由使用者明確勾選且記入審計日誌，**不允許靜默降級**

#### 2.2.2 Excel / CSV 處理模組（`excel/`）

> ⚠️ **格式修正**：calamine 僅支援 `.xlsx` / `.xls` / `.xlsb` / `.ods`，**不支援 CSV**。
> CSV 另以 `csv` crate 處理，並搭配 `encoding_rs` + `chardetng` 處理編碼。

- 讀取（試算表）：`calamine` — `.xlsx` / `.xls` / `.xlsb` / `.ods`
- 讀取（CSV）：`csv` crate，串流逐列；編碼自動偵測（`chardetng`）+ 手動指定選項（**UTF-8 / Big5(CP950) / UTF-16** — 台灣企業環境 Big5 為必測項目）
- 寫入：`rust_xlsxwriter` — 啟用 `constant_memory` 模式，匯出任意大小結果集記憶體用量恆定
- Schema 推斷：取樣前 100 行投票判斷型別；樣本中全為 NULL 的欄位標記為「未定」並要求使用者指定
- **記憶體限制（已知限制，如實揭露）**：calamine 會將整個 sheet 載入記憶體，xlsx 並非真正串流。對策：
  1. 開檔時依檔案大小與行數估算記憶體，超過閾值（預設 50 萬行或 200MB）顯示警告並要求確認
  2. 載入後以批次（每批 5,000 行）餵入 ETL 管線，避免二次複製
  3. 後續版本評估 SAX-style xlsx 串流 parser（列入 backlog，不阻塞 MVP）

#### 2.2.3 ETL 引擎模組（`etl/`）

- Mapping 引擎：定義 Excel 欄位 → DB 欄位的轉換規則
- 型別轉換：依 §4.3 型別矩陣自動 cast + 錯誤回報（行號、欄位名稱、原始值、失敗原因）
- 平行處理：`rayon par_iter` 多執行緒 transform（僅 CPU-bound 轉換階段；I/O 維持 Tokio）
- 批次寫入：依 §2.2.1 各 DB 最佳化機制
- 執行語意（交易、checkpoint、錯誤政策）：見 §4.4，為 P0 正式設計

### 2.3 專案目錄結構

```
elu-etl/
├── src-tauri/                        # Rust 後端
│   ├── Cargo.toml
│   ├── capabilities/                 # Tauri 2 權限設定（最小權限原則）
│   │   └── default.json
│   └── src/
│       ├── main.rs
│       ├── commands/                 # Tauri IPC 指令層（薄層，只做參數驗證與轉發）
│       │   ├── excel.rs
│       │   ├── database.rs
│       │   └── etl.rs
│       ├── db/                       # DB 連線模組
│       │   ├── mod.rs
│       │   ├── driver.rs             # DbDriver trait（統一抽象）
│       │   ├── pool.rs               # 連線池快取（AppState）
│       │   ├── mssql.rs              # tiberius 實作
│       │   ├── postgres.rs           # sqlx 實作
│       │   ├── mysql.rs
│       │   └── sqlite.rs
│       ├── excel/                    # Excel / CSV 讀寫模組
│       │   ├── reader.rs             # calamine 封裝
│       │   ├── csv_reader.rs         # csv + encoding_rs（Big5 等）
│       │   ├── writer.rs             # rust_xlsxwriter（constant memory）
│       │   └── schema_infer.rs       # 型別推斷
│       ├── etl/                      # ETL 核心邏輯
│       │   ├── mapping.rs
│       │   ├── transform.rs
│       │   ├── loader.rs
│       │   ├── executor.rs           # 任務編排、取消、進度
│       │   └── checkpoint.rs         # 續跑狀態（本地 SQLite）
│       ├── security/
│       │   ├── secrets.rs            # OS keychain 封裝（keyring）
│       │   └── redact.rs             # 日誌 / 錯誤訊息遮罩
│       ├── state/                    # 本地狀態庫（任務歷史、checkpoint、設定）
│       │   └── store.rs
│       ├── telemetry/                # tracing 初始化、審計日誌、Sentry
│       │   └── mod.rs
│       └── models/                   # 共用資料結構
│           ├── connection.rs
│           ├── schema.rs
│           └── errors.rs
├── src/                              # Angular 前端
│   └── app/
│       ├── pages/
│       │   ├── connections/          # 連線管理
│       │   ├── mapping/              # 欄位對應
│       │   └── execute/              # ETL 執行
│       ├── services/
│       │   └── tauri.service.ts      # Tauri invoke 封裝
│       ├── i18n/                     # zh-Hant / en（UI 雙語）
│       └── shared/
│           ├── data-grid/
│           └── progress-bar/
├── package.json
└── tauri.conf.json
```

**本地資料存放位置**（依 OS app data dir，經 `tauri::path` API 取得）：
- 連線設定（不含密碼）與 Mapping 範本：`{appConfigDir}/`（JSON，tauri-plugin-store）
- 密碼 / 連線機密：**OS keychain**（Windows Credential Manager / macOS Keychain / Linux Secret Service）
- 任務歷史、checkpoint：`{appDataDir}/state.db`（SQLite）
- 日誌：`{appLogDir}/`（滾動檔案，保留 30 天）

---

## 3. 技術選型 Tech Stack

### 3.1 Rust 依賴（`Cargo.toml`）

> 版本以開發啟動當下最新穩定版為準（`cargo add` 取得），下列為撰寫時之參考值。

```toml
[dependencies]
# 框架
tauri = { version = "2", features = [] }
tauri-plugin-dialog = "2"
tauri-plugin-fs = "2"
tauri-plugin-store = "2"
tauri-plugin-updater = "2"

# 非同步執行時
tokio = { version = "1", features = ["full"] }
tokio-util = "0.7"            # CancellationToken、tiberius compat
async-trait = "0.1"
futures = "0.3"

# 資料庫（pure Rust，不需 ODBC）
# 注意：sqlx 0.7+ 已無 mssql feature，SQL Server 一律走 tiberius
sqlx = { version = "0.8", features = [
    "runtime-tokio",
    "tls-rustls",
    "postgres",
    "mysql",
    "sqlite",
    "chrono",
    "rust_decimal",
] }
tiberius = { version = "0.12", default-features = false, features = [
    "rustls",
    "chrono",
    "tds73",
] }
deadpool = "0.12"             # tiberius 連線池

# Excel / CSV
calamine = "0.26"
rust_xlsxwriter = { version = "0.79", features = ["constant_memory"] }
csv = "1.3"
encoding_rs = "0.8"           # Big5 / UTF-16 解碼
chardetng = "0.1"             # 編碼自動偵測

# 效能
rayon = "1.10"

# 工具
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
chrono = { version = "0.4", features = ["serde"] }
rust_decimal = "1"            # 精確小數（金額），避免 f64 誤差
uuid = { version = "1", features = ["v4"] }
sha2 = "0.10"                 # 來源檔 hash（續跑驗證）

# 觀測性（取代 log/env_logger，企業級結構化日誌）
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
tracing-appender = "0.2"
sentry = { version = "0.34", features = ["tracing"] }

# 機密管理
keyring = "3"

[dev-dependencies]
testcontainers = "0.23"       # 整合測試：起 MSSQL / PG / MySQL 容器
criterion = "0.5"             # 效能基準
tempfile = "3"

[build-dependencies]
tauri-build = { version = "2", features = [] }
```

| Crate | 用途 | 授權 |
|---|---|---|
| tauri 2 | 桌面應用框架、WebView 整合 | MIT/Apache |
| tokio | 非同步執行時 | MIT |
| sqlx 0.8 | PostgreSQL / MySQL / SQLite 非同步查詢 | MIT/Apache |
| **tiberius** | **SQL Server TDS 驅動（主選，非備選）** | MIT/Apache |
| deadpool | tiberius 連線池 | MIT/Apache |
| calamine | Excel 讀取（xlsx/xls/xlsb/ods） | MIT |
| csv + encoding_rs | CSV 讀取 + Big5/UTF-16 編碼 | MIT/Apache |
| rust_xlsxwriter | Excel 寫入（constant memory 模式） | MIT/Apache |
| rayon | CPU 密集平行處理 | MIT/Apache |
| rust_decimal | 精確小數運算 | MIT |
| tracing 全家桶 | 結構化日誌 + 審計 | MIT |
| keyring | OS keychain 機密儲存 | MIT/Apache |
| thiserror 2 | 自訂錯誤型別 | MIT/Apache |

### 3.2 前端套件（Angular）

- **Angular 版本**：採用當前 **LTS 版**（撰寫時為 Angular 20 LTS；不指定舊版 17）
- **Node.js**：22 LTS 以上

```bash
npm install @tauri-apps/api @tauri-apps/plugin-dialog @tauri-apps/plugin-fs
npm install ag-grid-community ag-grid-angular
npm install -D tailwindcss
```

| 套件 | 用途 |
|---|---|
| @tauri-apps/api | Tauri IPC invoke / Channel 呼叫 |
| @tauri-apps/plugin-dialog | 開啟檔案對話框 |
| @tauri-apps/plugin-fs | 檔案系統存取（以 capabilities 限縮範圍） |
| TailwindCSS | 樣式框架 |
| AG Grid Community | 高效能表格元件（MIT 免費版） |
| @angular/localize | UI 國際化（zh-Hant / en） |
| RxJS | 非同步資料流處理（Angular 內建） |

---

## 4. 功能規格 Feature Specification

### 4.1 核心功能 MVP

| 功能模組 | 功能描述 | 優先級 |
|---|---|---|
| 資料庫連線管理 | 新增/編輯/測試連線（4 種 DB）、密碼存 OS keychain | P0 |
| Excel/CSV 匯入 | 拖放上傳、Sheet 選擇、編碼選擇（CSV）、預覽前 100 行、自動偵測型別 | P0 |
| 欄位 Mapping | 視覺化拖拉對應、型別轉換規則設定、Null 值處理 | P0 |
| ETL 執行 | 即時進度、錯誤行列出、**可取消、可續跑（見 §4.4）**、完成統計 | P0 |
| Excel 匯出 | SQL 查詢結果匯出為 xlsx（Rust 端串流，不限大小）、含格式化 | P0 |
| Mapping 範本 | 儲存/載入 Mapping 設定檔（JSON，含 schema version 供升版遷移） | P1 |
| Log / 審計紀錄 | 操作歷史、錯誤日誌、匯出報告（見 §4.6） | P1 |
| 排程執行 | 定時自動執行 ETL 任務（**架構註記**：需常駐 tray 程序或註冊 OS 排程器〔Task Scheduler / launchd / systemd timer〕，影響 app 生命週期設計，P2 實作但 Phase 1 架構需預留 headless 執行入口） | P2 |

### 4.2 效能指標目標

| 指標 | 目標值 | 實現方式 / 備註 |
|---|---|---|
| Excel 讀取速度 | > 50,000 行/秒 | calamine + rayon 平行解析 |
| DB 批次寫入 | > 10,000 行/秒 | Bulk Load / COPY；**以區網 DB 為基準**，跨網路依頻寬折減 |
| 應用程式啟動 | < 3 秒 | Tauri 系統 WebView，無打包 Chromium |
| 記憶體（Rust 核心 idle） | < 50 MB | 不含 WebView 程序 |
| 記憶體（整體含 WebView） | < 250 MB | Angular + AG Grid 載入後之實際可達值 |
| 安裝包大小 | < 10 MB | 使用系統 WebView（Windows 另需 WebView2 runtime，安裝器自動引導） |

> 效能驗收一律以 **100 萬行測試資料集** 於 CI 效能管線實測，不以理論值驗收。

### 4.3 型別轉換矩陣（Excel → DB）

ETL 工具最常見的缺陷來源即型別轉換，故明文定義：

| Excel 推斷型別 | Rust 中介型別 | SQL Server | PostgreSQL | MySQL | SQLite |
|---|---|---|---|---|---|
| 整數 | `i64` | BIGINT | BIGINT | BIGINT | INTEGER |
| 浮點數 | `f64` | FLOAT(53) | DOUBLE PRECISION | DOUBLE | REAL |
| 精確小數（金額） | `rust_decimal::Decimal` | DECIMAL(p,s) | NUMERIC | DECIMAL | TEXT |
| 文字 | `String` | NVARCHAR | TEXT | VARCHAR/TEXT | TEXT |
| 布林 | `bool` | BIT | BOOLEAN | TINYINT(1) | INTEGER |
| 日期時間 | `NaiveDateTime` | DATETIME2 | TIMESTAMP | DATETIME | TEXT (ISO 8601) |
| 日期 | `NaiveDate` | DATE | DATE | DATE | TEXT (ISO 8601) |
| NULL | `Option<T>` | 依 Mapping 規則：寫 NULL / 預設值 / 視為錯誤 | | | |

**Excel 日期特別規則**（必須在單元測試中覆蓋）：
- Excel 日期實為 f64 序號，需處理 **1900 / 1904 兩種紀元**（舊 Mac 檔案為 1904）
- Excel 1900 閏年 bug（1900-02-29 不存在但序號存在）依 calamine 慣例處理
- 時間一律視為 **naive local time，不做時區轉換**（明文記錄於使用手冊；DB 端 `timestamptz` 欄位需使用者明確指定時區）
- 轉換失敗（溢位、精度損失、無效日期）一律進錯誤報告，**絕不靜默截斷**

### 4.4 ETL 執行語意（P0 正式設計）

#### 寫入模式（使用者可選）

| 模式 | 行為 | 適用情境 |
|---|---|---|
| 批次提交（**預設**） | 每批 5,000 行一個交易，commit 後寫入 checkpoint | 大量資料、可容忍部分完成 |
| 全有全無 | 整個任務單一交易，任何錯誤全部 rollback | 中小量資料（≤ 50 萬行）、強一致需求 |
| Staging 合併（P1） | 先寫入暫存表，驗證通過後 `MERGE` / `INSERT … SELECT` 原子合併 | 可重跑（冪等）、正式環境匯入 |

#### 錯誤政策（使用者可選）

1. **跳過並記錄**（預設）：錯誤行寫入錯誤報告，繼續執行
2. **首錯即停**：遇第一筆錯誤即中止（依寫入模式決定 rollback 範圍）
3. **錯誤率閾值**：錯誤率超過 N%（預設 10%）自動中止，防止 mapping 設錯造成大規模垃圾寫入

#### 取消與續跑（Checkpoint / Resume）

- **取消**：`tokio_util::sync::CancellationToken` 協作式取消；當前未 commit 的批次 rollback，任務狀態記為 `cancelled`
- **Checkpoint**：每批 commit 後，將 `(job_id, 來源檔 SHA-256, 批次序號, 已寫入行數)` 寫入本地 `state.db`
- **續跑**：僅允許在「批次提交」模式下續跑；續跑前驗證來源檔 SHA-256 一致，否則拒絕並要求重新執行；從最後成功批次的下一批開始
- **冪等性界限（如實揭露）**：批次提交模式的續跑保證「不漏」，「不重」依賴 checkpoint 與 commit 的順序保證（先 commit 後記 checkpoint，故極端崩潰下最多重複一批）；需要嚴格不重不漏者引導使用 Staging 合併模式

#### 進度回報

Tauri 2 `ipc::Channel<EtlProgress>` 推送：批次序號、累計成功/失敗行數、吞吐（行/秒）、預估剩餘時間、當前階段（read / transform / load）。

### 4.5 安全性需求（企業基線）

| 領域 | 要求 |
|---|---|
| 機密管理 | DB 密碼僅存 **OS keychain**（keyring crate）；設定檔 JSON 僅存非機密欄位；**禁止自行實作加密** |
| 機密外洩防護 | 連線字串、密碼在日誌 / 錯誤訊息 / Sentry 事件中一律經 `redact` 模組遮罩；錯誤報告預設不含資料內容（可選開啟，需確認對話框） |
| 傳輸安全 | 全部 TLS（rustls）；MSSQL `encrypt=true` 預設；自簽憑證信任需明確 opt-in 並記入審計日誌 |
| 最小權限 | Tauri 2 capabilities 僅開放必要 API；`plugin-fs` 限縮至使用者選擇的檔案；嚴格 CSP；不載入任何遠端內容 |
| 審計日誌 | 結構化（JSON）記錄：連線建立/測試、ETL 任務啟動/完成/取消、設定變更，含時間戳與結果，本地保留 30 天 |
| 供應鏈安全 | CI 強制 `cargo-deny`（授權白名單：MIT/Apache/BSD + 漏洞資料庫）、`cargo audit`、`npm audit`；鎖定 `Cargo.lock` / `package-lock.json`；Renovate 自動升版 PR；發佈時產出 SBOM（CycloneDX） |
| 更新安全 | tauri-plugin-updater 更新包以 minisign 簽章驗證；私鑰離線保管 + CI 以 GitHub Environments secrets 注入；公鑰內嵌於 app |
| 崩潰回報 | Sentry（Rust + Angular），開啟 PII scrubbing，使用者可於設定中停用 |

### 4.6 關鍵程式碼範例

#### 連線池集中管理（修正：禁止在 command 內建池）

```rust
// src-tauri/src/db/pool.rs
pub struct AppState {
    /// ConnectionId → 驅動實例（內含連線池），跨 command 重用
    drivers: RwLock<HashMap<ConnectionId, Arc<dyn DbDriver>>>,
}

// src-tauri/src/commands/database.rs
#[tauri::command]
pub async fn execute_query_preview(
    state: tauri::State<'_, AppState>,
    conn_id: ConnectionId,
    sql: String,
) -> Result<QueryPreview, EluEtlError> {
    let driver = state.driver(&conn_id).await?;   // 取得既有池
    driver.query_preview(&sql, 100).await          // 僅回傳前 100 行跨 IPC
}
```

#### ETL 執行（取消 + Channel 進度）

```rust
// src-tauri/src/commands/etl.rs
#[tauri::command]
pub async fn execute_etl(
    state: tauri::State<'_, AppState>,
    job: EtlJobConfig,
    progress: tauri::ipc::Channel<EtlProgress>,
) -> Result<EtlSummary, EluEtlError> {
    let cancel = state.register_job(&job.id);      // CancellationToken
    etl::executor::run(&state, job, progress, cancel).await
}
```

#### Angular 呼叫（前端）

```typescript
// src/app/services/tauri.service.ts
import { invoke, Channel } from '@tauri-apps/api/core';

export class TauriService {
  async executeEtl(job: EtlJobConfig, onProgress: (p: EtlProgress) => void): Promise<EtlSummary> {
    const progress = new Channel<EtlProgress>();
    progress.onmessage = onProgress;
    return invoke<EtlSummary>('execute_etl', { job, progress });
  }
}
```

#### ETL 平行處理（Rust）

```rust
// src-tauri/src/etl/transform.rs
use rayon::prelude::*;

pub fn transform_batch(rows: &[ExcelRow], rules: &[MappingRule]) -> (Vec<DbRow>, Vec<EtlError>) {
    rows.par_iter()
        .map(|row| apply_rules(row, rules))
        .partition_map(|result| match result {
            Ok(db_row) => Either::Left(db_row),
            Err(e)     => Either::Right(e),
        })
}
```

---

## 5. 測試策略 Test Strategy

| 層級 | 範圍 | 工具 | 門檻 |
|---|---|---|---|
| 單元測試 | 型別轉換矩陣（含 Excel 日期紀元 / 閏年 bug / 溢位）、schema 推斷、mapping 規則、redact | `cargo test` + `cargo-llvm-cov` | 核心模組行覆蓋 ≥ 80%，PR gate |
| 整合測試 | 4 種 DB 真實讀寫、bulk insert、交易/rollback、checkpoint 續跑 | `testcontainers-rs`（MSSQL / PG / MySQL 容器；SQLite in-memory） | CI 必跑（Linux runner） |
| Golden file | Excel 讀寫雙向（含 Big5 CSV、1904 紀元 xls、混合型別） | 版本庫內測試資料集 | CI 必跑 |
| 前端單元 | TauriService、mapping 元件邏輯（IPC 以 mock 注入） | Vitest + Angular TestBed | PR gate |
| E2E | 連線 → 匯入 → mapping → 執行完整流程 | `tauri-driver` + WebdriverIO（**僅支援 Windows / Linux，macOS 不支援** — macOS 以 Playwright + mock IPC 跑 UI 流程補位） | 發版前必跑 |
| 效能 | 100 萬行資料集讀取 / 寫入吞吐、記憶體峰值 | criterion + 壓測腳本 | nightly CI，回歸 >10% 告警 |

**CI/CD 管線（GitHub Actions）**：
- PR gate：`cargo fmt --check`、`clippy -D warnings`、單元+整合測試、`cargo-deny`、`cargo audit`、前端 lint + test
- Build matrix：`windows-latest` / `macos-latest` / `ubuntu-22.04`（Linux 需安裝 `libwebkit2gtk-4.1-dev`、`libappindicator3-dev` 等系統依賴，寫入 workflow）
- Release（tag 觸發）：三平台建置 → 簽署（Windows）/ notarize（macOS）→ 產出 updater `latest.json` 與簽章 → GitHub Release + SBOM

---

## 6. 開發計畫 Development Plan

### Phase 0 — 環境建置與前置申請（第 1 週）

- [ ] 安裝 Rust toolchain（`rustup` + stable channel）
- [ ] 安裝 Node.js 22 LTS + Angular CLI（當前 LTS 版）
- [ ] 建立專案骨架：`npm create tauri-app@latest`（選 Angular 範本；`cargo tauri init` 無 Angular 範本）
- [ ] 設定 VS Code + rust-analyzer + Tauri 擴充套件
- [ ] 建立 Git repo，設定 `.gitignore`（`target/`, `dist/`, `node_modules/`）
- [ ] GitHub Actions 三平台 build matrix 通過（含 Linux webkit 系統依賴）
- [ ] CI 安全 gate 上線：cargo-deny / cargo audit / clippy / fmt
- [ ] **🔑 立即啟動：Windows 程式碼簽署申請**（Azure Trusted Signing 或 EV 憑證 — **前置時間數週**，最晚此週送件）
- [ ] **🔑 立即啟動：Apple Developer Program 註冊**（notarization 必需，$99/年）
- [ ] 產生 updater minisign 金鑰對，私鑰離線保管 + 寫入 CI secrets

### Phase 1 — Rust 核心開發（第 2–4 週）

#### Week 2：DB 連線層

- [ ] `DbDriver` trait 定義 + `ConnectionConfig` struct
- [ ] `secrets.rs`：keyring 封裝（存/取/刪密碼）+ `redact.rs` 日誌遮罩
- [ ] sqlx 驅動實作：PostgreSQL / MySQL / SQLite（含連線池）
- [ ] **tiberius 驅動實作：SQL Server（deadpool 連線池 + rustls TLS）**
- [ ] 連線池快取於 `AppState`（`ConnectionId` 為鍵）
- [ ] Tauri commands：`test_connection`、`execute_query_preview`、`get_tables`、`get_columns`
- [ ] 自訂 `EluEtlError`（thiserror 2 + 錯誤代碼），統一格式回傳前端
- [ ] 整合測試：testcontainers 起 4 種 DB，驗證 trait 行為一致
- [ ] tracing 初始化（JSON 滾動日誌 + 審計事件）

#### Week 3：Excel / CSV 處理層

- [ ] calamine 封裝：`open_workbook`、`list_sheets`、`read_rows`（批次餵送）
- [ ] 檔案大小 / 行數預檢與記憶體警告閾值
- [ ] CSV 讀取：`csv` + `chardetng` 自動偵測 + 手動編碼選項（**Big5 測試資料必備**）
- [ ] Schema 推斷演算法（前 100 行投票；全 NULL 欄位標記未定）
- [ ] Excel 日期處理：1900/1904 紀元、閏年 bug、serial → chrono（單元測試覆蓋）
- [ ] rust_xlsxwriter 封裝：`query_stream → xlsx`（constant memory，Rust 端串流匯出）
- [ ] Tauri commands：`read_preview`、`infer_schema`、`export_to_excel`
- [ ] Golden file 測試資料集建立（xlsx / xls(1904) / Big5 CSV / 混合型別）

#### Week 4：ETL 引擎

- [ ] `MappingRule` struct（來源欄、目標欄、型別轉換、null 行為）+ 範本 JSON schema version
- [ ] `transform_row`（單行轉換 + 錯誤收集，依 §4.3 矩陣）
- [ ] `rayon par_iter` 批次 transform
- [ ] `bulk_insert` 四種 DB 實作（Bulk Load / COPY / 多列 INSERT / 交易批次）
- [ ] 寫入模式：批次提交 / 全有全無；錯誤政策三選項（§4.4）
- [ ] `checkpoint.rs`：state.db schema、來源檔 SHA-256、續跑流程
- [ ] 取消機制：CancellationToken + 當前批次 rollback
- [ ] Tauri command：`execute_etl`（Channel 進度事件）、`resume_etl`、`cancel_etl`
- [ ] 整合測試：中斷→續跑、全有全無 rollback、錯誤率閾值中止

### Phase 2 — Angular 前端開發（第 5–6 週）

#### Week 5：基礎 UI

- [ ] TailwindCSS + AG Grid Community + @angular/localize（zh-Hant / en 骨架）
- [ ] `TauriService` 封裝（統一管理 invoke / Channel；IPC mock 介面供測試）
- [ ] `ConnectionPage`：連線管理 UI、測試連線、密碼進 keychain（UI 不回顯）
- [ ] `ExcelPage`：拖放上傳、Sheet / 編碼選擇、AG Grid 預覽、大檔警告對話框
- [ ] 前端單元測試（Vitest）同步建立

#### Week 6：ETL 流程 UI

- [ ] `MappingPage`：左欄（Excel Schema）拖拉對應右欄（DB Table）
- [ ] 型別轉換選單、Null 處理選項、寫入模式 / 錯誤政策選擇器
- [ ] `ExecutionPage`：即時進度（Channel）、錯誤列表（行號+原因）、取消按鈕、續跑入口
- [ ] 完成統計頁：成功/失敗筆數、耗時、吞吐、下載錯誤報告（xlsx）
- [ ] Mapping 範本儲存 / 載入

### Phase 3 — 整合測試與打包（第 7–8 週）

- [ ] E2E：tauri-driver + WebdriverIO（Windows / Linux）；macOS 以 Playwright + mock IPC 補 UI 流程
- [ ] 效能壓測：100 萬行 Excel → 4 種 DB，驗證 §4.2 指標、記憶體峰值記錄
- [ ] 三平台打包：Windows `.msi`（WiX）、macOS `.dmg`、Linux `.AppImage` + `.deb`
- [ ] 程式碼簽署接線：Windows（Phase 0 申請之憑證 / Trusted Signing）、macOS notarytool 流程進 CI
- [ ] 自動更新：tauri-plugin-updater + `latest.json` 託管（GitHub Releases），含降級防護驗證
- [ ] Windows WebView2 runtime 引導安裝測試（乾淨 VM）
- [ ] 異常情境手動測試：DB 斷線、磁碟滿、檔案被鎖定、睡眠喚醒

### Phase 4 — 商業化準備（第 9–10 週）

- [ ] License Key 驗證（後端 EluCloud.Api）：license 檔以 **Ed25519 簽章**、線上啟用 + **離線寬限期 14 天**、機器指紋（雜湊後）綁定
- [ ] 版本限制（免費版：100 行/次；專業版：無限制）
- [ ] Crash Report：Sentry Rust + Angular SDK，PII scrubbing 驗證、設定頁可停用
- [ ] 安裝程式精修（NSIS/WiX 選項、macOS notarization 全流程演練）
- [ ] SBOM 產出納入 release pipeline
- [ ] 說明文件（英文 + 繁體中文），含已知限制（xlsx 記憶體、時區規則、續跑語意）
- [ ] UI i18n 完成度檢查（zh-Hant / en）
- [ ] 發佈演練：tag → CI → 簽署 → updater 升版實測（舊版自動更新到新版）

---

## 7. 時程摘要 Timeline

| Phase | 時程 | 主要產出 | 狀態 |
|---|---|---|---|
| Phase 0 — 環境 + 憑證申請 | 第 1 週 | 骨架、CI gate、**簽署憑證送件** | ⬜ 待開始 |
| Phase 1 — Rust 核心 | 第 2–4 週 | DB（sqlx+tiberius）/ Excel / ETL 引擎 + 單元/整合測試 | ⬜ 待開始 |
| Phase 2 — Angular 前端 | 第 5–6 週 | 完整 UI 流程（連線→Mapping→執行→續跑） | ⬜ 待開始 |
| Phase 3 — 整合測試打包 | 第 7–8 週 | 三平台簽署安裝包、效能達標、自動更新 | ⬜ 待開始 |
| Phase 4 — 商業化 | 第 9–10 週 | License 機制、Sentry、文件、發佈演練 | ⬜ 待開始 |

**總計：10 週（約 2.5 個月），1 位全端開發者獨立完成 MVP。**
時程已含測試與打包的實際工作量；若簽署憑證核發延遲（不可控），Phase 3 簽署項目可後移至 Phase 4，不阻塞其餘工作。

---

## 8. 風險評估 Risk Assessment

| 風險項目 | 可能性 | 影響 | 緩解策略 |
|---|---|---|---|
| sqlx + tiberius 雙驅動抽象成本（行為差異） | 中 | 中 | `DbDriver` trait 統一介面；testcontainers 對 4 DB 跑同一套整合測試保證行為一致 |
| calamine 整檔載入記憶體（非串流） | 高 | 中 | 開檔預檢 + 警告閾值；批次餵送；backlog 評估串流 parser；文件如實揭露 |
| **簽署憑證核發前置時間（數週）** | 高 | 高 | **Phase 0 第 1 週即送件**；優先評估 Azure Trusted Signing（免實體憑證、核發快） |
| Linux webkit2gtk 渲染 / AG Grid 行為差異 | 中 | 中 | CI 含 Linux build；E2E 跑 Linux；發版前手動冒煙測試三平台 |
| tauri-driver E2E 不支援 macOS | 確定 | 中 | macOS 以 Playwright + mock IPC 覆蓋 UI 流程；核心邏輯由 Rust 整合測試保證 |
| CSV 編碼誤判（Big5 / UTF-8） | 中 | 中 | chardetng 自動偵測 + UI 手動指定 + 預覽即時重解碼確認 |
| 大檔案記憶體溢出 | 中 | 高 | 預檢警告 + 批次處理（每批 5,000 行）+ 匯出走 constant memory |
| 續跑語意誤解造成重複寫入 | 低 | 高 | 先 commit 後記 checkpoint；文件明示「極端崩潰最多重一批」；嚴格場景導向 Staging 模式 |
| Tauri 2 API 版本變動 | 低 | 中 | 鎖定 lockfile；Renovate 升版走 PR + CI 驗證 |
| 時程壓力（單人 10 週） | 中 | 中 | P1/P2 功能可裁切；每 Phase 結尾為可交付狀態（增量交付） |

---

## 9. 立即開始 Quick Start

### 9.1 環境安裝指令

```bash
# 1. 安裝 Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# 2. 建立專案（互動式選擇 Angular 前端範本）
npm create tauri-app@latest

# 3. 加入 Rust 核心依賴（注意：sqlx 不含 mssql，SQL Server 用 tiberius）
cargo add sqlx --features runtime-tokio,tls-rustls,postgres,mysql,sqlite,chrono,rust_decimal
cargo add tiberius --no-default-features --features rustls,chrono,tds73
cargo add deadpool tokio --features tokio/full
cargo add tokio-util async-trait futures
cargo add calamine rust_xlsxwriter csv encoding_rs chardetng
cargo add rayon serde serde_json thiserror chrono rust_decimal uuid sha2 keyring
cargo add tracing tracing-subscriber tracing-appender

# 4. 加入 Angular / Node 套件
npm install @tauri-apps/api @tauri-apps/plugin-dialog @tauri-apps/plugin-fs
npm install ag-grid-community ag-grid-angular
npm install -D tailwindcss

# 5. （Linux 開發機）安裝 webkit 系統依賴
sudo apt install libwebkit2gtk-4.1-dev build-essential libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev

# 6. 啟動開發模式（熱重載）
cargo tauri dev
```

### 9.2 第一週 Checklist

- [ ] Rust + Cargo 安裝完成，`cargo --version` 正常輸出
- [ ] 專案骨架建立，`cargo tauri dev` 可啟動空白視窗
- [ ] Angular 基本路由設定（`/connections`, `/mapping`, `/execute`）
- [ ] 第一個 Tauri command（`hello_world`）可從 Angular `invoke` 呼叫成功
- [ ] Git 初始化，推送至 GitHub
- [ ] GitHub Actions 三平台 build matrix 全部通過（含 Linux 系統依賴）
- [ ] CI gate（fmt / clippy / cargo-deny / cargo audit）全綠
- [ ] **Windows 簽署（Azure Trusted Signing 或 EV 憑證）已送件**
- [ ] **Apple Developer Program 已註冊**
- [ ] updater minisign 金鑰已產生並離線備份

### 9.3 推薦 VS Code 擴充套件

```json
{
  "recommendations": [
    "rust-lang.rust-analyzer",
    "tauri-apps.tauri-vscode",
    "bradlc.vscode-tailwindcss",
    "angular.ng-template",
    "usernamehw.errorlens"
  ]
}
```

---

> 本計畫書由伊露科技有限公司內部使用。技術選型以商業友善授權（MIT/Apache 2.0）為優先，確保產品可閉源商業化而無授權風險。
> SQL Server 採 tiberius（sqlx 0.7+ 已無 MSSQL 支援）；Slint 因 GPL 授權問題排除；egui 因 Immediate Mode 持續耗電問題排除。
> 文中依賴版本為撰寫當下參考值，開發啟動時以最新穩定版為準並鎖定 lockfile。
