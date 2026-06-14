# Elu ETL — DSL 重構規格（v0.1 → v0.2）

> 給 Claude Code 當改動依據。涵蓋四件事：**(1) DSL 語法收斂**、**(2) 新舊差異與 bug 修正**、**(3) parser / executor 語意規則**、**(4) GUI 調整（對照現有截圖）**，最後附 **(5) CodeMirror 高亮（含字串插值）**。
>
> 本文件對齊現況：純 Rust 驅動（tiberius / sqlx）、Tauri 2 + Angular 20、`/works` 視覺化編輯器與 DSL 編輯器**雙向同步**、`.etl` 檔自包含 `SOURCE` / `TARGET` 宣告、執行採批次交易 + checkpoint。**所有 DSL 改動都必須能 round-trip 回視覺化編輯器**——這是最硬的約束。

---

## 1. 設計總綱（三個核心決定）

### 1.1 子句化（clause-based）
工作主體由一組有序子句構成，取代舊版把「比對條件 / 寫入目標 / 動作」混在一起、靠位置和上下文猜語意的寫法：

```
FROM → JOIN* → WHERE? → INTO → (ON → MATCHED? / NOT MATCHED?)? → ADD / UPDATE
```

執行心智流順著「**讀來源 → 查表 → 過濾 → 指定目標 → 比對 → 動作**」，與 SQL 直覺一致。

### 1.2 兩種「對應」必須分開（本次重構的核心）

現有工具與 GUI 只建模了**第一種**；第二種是這次要新增的能力。

| 種類 | 語意 | 關鍵字 | 截圖對應 |
|---|---|---|---|
| **查表 / Lookup join** | 拿來源欄位去比**另一張表**取值（來源 ↔ 來源/查表） | `JOIN <別名> = <連線>.[…] ON (<條件>)` | 現有「比對資料表 / 比對欄位」 |
| **合併鍵 / Merge key** | 判斷**目標表**裡是否已有同一筆，決定 insert/update/skip（目標 ↔ 來源） | 頂層 `ON (<條件>)` + `MATCHED` / `NOT MATCHED` | **目前 GUI 沒有，需新增** |

> 兩處都讀作「條件」，但角色不同：`JOIN … ON` 是 join key（少一筆 = 查無對應）；頂層 `ON` 是 merge key（少一筆 = 目標尚未存在 → 該寫入）。

### 1.3 命名收斂表

| 舊（對話中出現過 / 你貼的版本） | 收斂後 | 原因 |
|---|---|---|
| `JUDGE = [ … ]` | `FROM` / `JOIN` | `JUDGE` 放的是「要讀的資料」，不是在判斷 |
| `JUDGE[0]` / `JUDGE[1]`（位置索引） | 具名別名 `entra` / `account` / `mapping` | 自我說明、對順序不脆弱、可擴充 |
| `EXECUTE = <目標表>` | `INTO` | `EXECUTE` 放的是寫入目標，不是在執行 |
| `INTO … AS <別名>`（提案曾用） | `INTO <別名> = …` | 別名語法全面統一為 `別名 = 來源`，與 `FROM`/`JOIN` 一致，不與 `AS` 並存 |
| `MATCH ( … )` | 查表用 `JOIN … ON`；對目標去重用頂層 `ON` | `MATCH` 與分支字 `MATCHED` 字根撞、且把兩種對應混為一談 |
| `If`（同時被當 filter / 存在檢查 / 條件分支） | filter → `WHERE`；存在檢查 → `ON` + `NOT MATCHED`；真正 row-level → 仍可保留 `IF`（少用） | 一個關鍵字扛三種語意是舊版最大可讀性債 |

`JUDGE` / `EXECUTE` / `MATCH` / 舊式 `If … 換行 [表] 換行 ADD` **保留相容解析**，但 parser 一律正規化為新 AST（與現有「`'前綴' + [欄位]` 正規化為模板」同一套哲學），使視覺化編輯器永遠只面對新模型。

---

## 2. 關鍵字與運算子總表

### 結構
| 語法 | 說明 |
|---|---|
| `WORK '<名稱>' { … }` | 定義一個轉換工作單元 |
| `GO` | 結束並送出 batch（同 T-SQL；舊式 `GO` 分隔仍相容） |

### 連線宣告（標頭，順序不拘、皆選擇性）
| 語法 | 說明 |
|---|---|
| `SOURCE = CONNECTION('<名稱>' [, TABLE='…' \| QUERY='…'])` | 來源為已儲存連線 |
| `SOURCE = FILE(PATH='…' [, TYPE=…, SHEET='…', ENCODING='…', HEADER=TRUE\|FALSE])` | 來源為檔案（Excel / CSV） |
| `TARGET = CONNECTION('<名稱>')` | 目標連線（僅引用已儲存連線，密碼留 keychain；檔案不可當目標） |

### 資料流子句（建議順序）
| 語法 | 說明 |
|---|---|
| `FROM <別名> = <連線>.[…]` | **主來源**：逐列迭代的對象（決定 body 跑幾次） |
| `JOIN <別名> = <連線>.[…] ON (<條件>)` | 查表（0..N 個）。預設 inner（未命中 → 錯誤報告），見 §5.2 |
| `WHERE <條件>` | 過濾來源/已 join 的列 |
| `INTO [<別名> =] <連線>.[…]` | 寫入目標；要用合併鍵時取別名供 `ON` 引用（別名語法 `別名 = 來源`，與 `FROM`/`JOIN` 一致） |

### 比對與分支（MERGE 語意；整段可省略 → `ADD` 視為純附加）
| 語法 | 說明 |
|---|---|
| `ON (<條件>)` | 定義「同一筆」的合併鍵（目標別名 ↔ 來源/查表別名） |
| `NOT MATCHED { <動作> }` | 目標查無符合列時 |
| `MATCHED { <動作> }` | 目標已存在符合列時 |

### 動作
| 語法 | 說明 |
|---|---|
| `ADD { 欄位 = 值, … }` | 新增（INSERT） |
| `UPDATE { 欄位 = 值, … }` | 更新 |
| `SKIP` | 略過不動作 |
| `DELETE` | 刪除（選用） |

### 判斷式 / 運算子
| 符號 / 字 | 說明 |
|---|---|
| `==` `!=` `>` `<` `>=` `<=` | 比較 |
| `&&` `\|\|` `!` | AND / OR / NOT。⚠️ **`!!` 是雙重否定會還原，禁止用來表達「不存在」** |
| `IS EMPTY` / `IS NOT EMPTY` | 空值/空字串判斷（取代會踩三值邏輯的 `= NULL`） |
| `IN` / `LIKE` / `BETWEEN` | 過濾用（選用） |
| `=` | 指派（別名宣告、`ADD`/`UPDATE` 欄位） |
| `.` `,` | 成員存取 / 分隔 |
| `( )` `{ }` `[ ]` | 群組 / 區塊 / 中括號識別字 |

### 產生器 / 字面值
| 語法 | 說明 |
|---|---|
| `Gen.GUID` / `Gen.GUID(Text)` | UUID v4，每列新值 |
| `Gen.ULID` | 26 字元 Crockford base32，時間排序友善 |
| `Gen.Date` / `Gen.DateTime` / `Gen.DateTimeOffset`（含 `(Text)`） | 執行當下時間，同次執行所有列一致 |
| `Gen.SHA256` / `Gen.SHA512` / `Gen.MD5` | 來源整列雜湊（hex 小寫） |
| `N'…'` / `'…'` | Unicode / 一般字串；`''` 為跳脫單引號 |
| `NULL` / `TRUE` / `FALSE` | 空值 / 布林 |
| `[識別字]` | 中括號識別字，如 `[dbo].[DirectoryAccounts]` |
| `{…}`（字串內） | **插值**，如 `N'LDAP: {account.[DisplayName]}'`；字面大括號以 `{{` / `}}` 跳脫；`NULL` 視為空字串 |

> 關鍵字一律不分大小寫；`--` 或 `//` 單行註解，`///` … `///` 多行註解；識別字可用 `[名稱]` 或裸字。

---

## 3. 兩種典型工作的標準寫法（canonical）

### 3.1 查表 + 新增：CSV/Entra → EluCloud DB（lookup join）

```sql
// 來源：Entra 匯出的 CSV；比對表 DirectoryAccounts 在 EluCloud 資料庫
SOURCE = CONNECTION('Entra ID')      // 此連線實際指向 CSV 匯出
TARGET = CONNECTION('EluCloud')

WORK 'EluCloudAccount綁定EnterId' {
  FROM entra   = SOURCE.[users]                          // 主來源：逐列迭代
  JOIN account = TARGET.[dbo].[DirectoryAccounts]        // 查表（在 TARGET 連線）
    ON (entra.[userPrincipalName] == account.[Email])    // join key；未命中 → 錯誤報告

  INTO TARGET.[dbo].[ExternalIdentityMappings]
  ADD {
     [Id]                 = Gen.ULID
    ,[AccountId]          = account.[Id]
    ,[ExternalId]         = entra.[id]
    ,[ExternalSystemType] = N'MICROSOFT_ENTRA_ID'
    ,[Label]              = N'MICROSOFT_ENTRA_ID: {account.[DisplayName]}'
  }
}
GO
```

**與你貼的「新版」差異**：把 `FROM { entra=…, account=… }`（多綁定 FROM）拆成 `FROM`（主來源）+ `JOIN`（查表），並把 `MATCH` 換成 `JOIN … ON`。理由見 §1.2——`account` 是查表而非驅動來源，分開後「誰決定列數」一目了然，也與 §3.2 的 merge `ON` 不再混淆。

> 註：CSV 來源是單一工作表，`SOURCE.[users]` 的實體名為**名目用途**；若覺得誤導可寫 `FROM rows = SOURCE` 或用實際工作表名。讓實體名與連線型別對得上（不要 CSV 卻叫 `sheet1`）。

### 3.2 條件去重 + 新增：DB → DB（merge / upsert）

此版已正確，維持不動：

```sql
SOURCE = CONNECTION('EluCloud')
TARGET = CONNECTION('EluCloud')

WORK 'DirectoryAccount綁定LDAP' {
  FROM  account = SOURCE.[dbo].[DirectoryAccounts]
  WHERE account.[LdapId] IS NOT EMPTY            // 沒有 LdapId 的帳號不處理

  INTO  mapping = TARGET.[dbo].[ExternalIdentityMappings]
  ON ( mapping.[ExternalSystemType] == N'LDAP' &&
       mapping.[ExternalId]         == account.[LdapId] )   // 合併鍵：什麼叫「同一筆」

  NOT MATCHED {                                  // 目標尚無此筆才寫入（idempotent）
    ADD {
       [Id]                 = Gen.ULID
      ,[AccountId]          = account.[Id]
      ,[ExternalId]         = account.[LdapId]
      ,[ExternalSystemType] = N'LDAP'
      ,[Label]              = N'LDAP: {account.[DisplayName]}'
    }
  }
}
GO
```

---

## 4. 新舊差異對照與 bug 修正

下列幾項在舊寫法下會**默默出錯、不報錯**，務必修掉：

| # | 舊寫法 | 問題 | 修正 |
|---|---|---|---|
| 1 | `[dbo].[ExternalIdentityMappings]` 夾在 `If` 與 `ADD` 中間 | 無關鍵字標示寫入目標，人與 parser 都得猜 | `INTO TARGET.[…]` |
| 2 | `JUDGE[0]` / `JUDGE[1]` 位置索引 | 重排來源 → 全錯且不報錯 | 具名別名 `entra` / `account` / `mapping` |
| 3 | `MATCH ( … )` | 把「查表」與「對目標去重」混為一談 | 查表 `JOIN … ON`；去重 `ON` + `NOT MATCHED` |
| 4 | `!! (EXECUTE.… == …)` | **`!!` 雙重否定 = 還原**，語意正好相反（變成「已存在才寫」） | `NOT MATCHED { … }`（別把存在檢查寫成布林） |
| 5 | `N'… {SOURCETABLE.DisplayName}'` | `SOURCETABLE` 是**懸空別名**，無處定義 | `{account.[DisplayName]}` |
| 6 | 在 `If` 裡讀 `EXECUTE.ExternalSystemType` 等目標欄位做判斷 | 把寫入目標當來源讀——這個「味道」就是該用 merge 的訊號 | merge `ON` + `MATCHED`/`NOT MATCHED` |
| 7 | `LdapId != NullOrEmpty` / 落地成 `LdapId != NULL` | `NullOrEmpty` 非可比值；且 SQL 三值邏輯下 `≠ NULL` 永遠 UNKNOWN → **一筆都過不了** | `account.[LdapId] IS NOT EMPTY` |
| 8 | `JUDGE[1].[Email]`（有括號）與 `JUDGE[1].Id`（無括號）、`id`/`Id` 混用 | 風格不一致 | 一律中括號識別字、大小寫統一 |
| 9 | `If` 同時當 filter / 存在檢查 / row-level 條件 | 三種語意擠在一個關鍵字 | `WHERE`（filter）/ `ON`+`NOT MATCHED`（存在）/ `IF`（真正 row-level，罕用） |

---

## 5. parser / executor 語意規則

對應 `src-tauri/src/etl/script/`（手寫 lexer + recursive descent、AST、executor）。

### 5.1 AST 節點（新增 / 調整）
- `Work { name, from, joins: Vec<Join>, where_: Option<Predicate>, into, merge: Option<Merge>, action: Action }`
- `Binding { alias: String, conn: ConnRef /* SOURCE|TARGET */, table: QualifiedName }`
- `From(Binding)` — **恰一個**主來源
- `Join { binding: Binding, on: Condition, policy: JoinPolicy /* Inner | Left */ }`
- `Into { conn: ConnRef, table: QualifiedName, alias: Option<String> }`
- `Merge { on: Condition, matched: Option<Action>, not_matched: Option<Action> }`
- `Action { Add(Vec<Assignment>) | Update(Vec<Assignment>) | Skip | Delete }`
- `ValueSource { Gen(GenKind) | Field { alias: String, column: String } | Literal(Value) | Template(Vec<TemplatePart>) }`
  - **關鍵改動**：`Field` 改帶 `alias`（`entra`/`account`/`mapping`），不再是裸表路徑。`Template` 的動態段也用 alias-qualified 欄位。

### 5.2 執行語意
- **`FROM`**：主來源逐列迭代，決定 body 執行次數。
- **`JOIN`**：維持現有 hash lookup（文字不分大小寫），但支援多張 + policy：
  - `Inner`（預設）：未命中 → 該列進**錯誤報告**（＝現況「啟用比對」勾選時的行為）。
  - `Left`（選用）：未命中 → 該查表欄位取 `NULL` 照常寫。
- **`WHERE`**：在進入動作前過濾列。
- **`INTO x = …` + `ON`（merge）**：
  - 建議實作為**預載目標既有鍵集合**到 `HashSet`（例如所有 `(ExternalSystemType, ExternalId)`）後逐列 probe，避免每列一次 `EXISTS` 查詢的 N+1。
  - `NOT MATCHED` → INSERT（走現有批次寫入 + checkpoint）。`MATCHED` → UPDATE / SKIP。
- **無 `ON` / 無 `MATCHED`/`NOT MATCHED`**：`ADD` 退化為純 append（＝現況「If 可省略，全部插入」）。
- **相容**：舊式 `If … 換行 [表] 換行 ADD`、`JUDGE`/`EXECUTE`/`MATCH` 解析後正規化為上述 AST。**`AS` 不屬於語法**；別名一律 `別名 = 來源`，若解析到 `INTO … AS x` 應直接給診斷提示改用 `INTO x = …`（而非默默接受，以免兩套語法並存）。

### 5.3 必記的 caveat
merge `ON` 只能去重「**目標表已存在**」的列，**擋不住同一批來源裡兩列共用同一鍵**（例如兩個 `DirectoryAccounts` 撞同一 `LdapId`）——兩列都會通過 `NOT MATCHED` 雙雙寫入。若來源端不保證唯一，需在 `FROM`/`JOIN` 層加來源去重（或讓 `ON` 涵蓋此語意）。文件先標記位置，是否實作由你決定。

---

## 6. GUI 調整（對照現有截圖）

### 6.1 現況（截圖 `/works` 視覺化編輯器）
中間「**判斷邏輯**」面板只建模了**單一查表**：來源欄位 `userPrincipalName` `==` 比對資料表 `dbo.DirectoryAccounts` 的比對欄位 `Email`，加一個 checkbox「啟用比對（IF 命中才寫入，未命中進錯誤報告）」。右側「**執行作業**」是目標表 + 欄位對應表（目標欄位 / 值來源 / 值）。

**缺口**：無法表達 (a) 多張查表、(b) `WHERE` 過濾、(c) **對目標的合併鍵 + MATCHED/NOT MATCHED**。下面逐塊改。

### 6.2 中間面板「判斷邏輯」→ 拆成兩段

```
┌─ 來源與關聯（FROM / JOIN）────────────────┐
│ 主來源    [entra] = [SOURCE ▾].[users        ]│   ← 別名可編輯
│ ─────────────────────────────────────────── │
│ 關聯表 ①  [account] = [TARGET ▾].[dbo.DirectoryAccounts]│
│   條件    entra.[userPrincipalName] == account.[Email] │
│   未命中  ( ) 進錯誤報告(inner)  ( ) 取空值照寫(left)   │   ← 取代舊 checkbox
│   [＋ 新增關聯表]                                       │
└──────────────────────────────────────────────┘
┌─ 篩選（WHERE）─────────────────────────────┐
│ account.[LdapId]  [IS NOT EMPTY ▾]              │   ← 運算子下拉：== != IS EMPTY IS NOT EMPTY …
│ [＋ 新增條件]   （多條以 AND 串接）             │
└──────────────────────────────────────────────┘
```

- 「比對資料表 / 比對欄位」從**單一**升級為**關聯表清單**（每張一組別名 + ON 條件 + 未命中政策）。
- 「主來源」明確獨立出來（取代頂部工具列來源選擇器的隱含角色）。
- 新增「篩選」區塊對應 `WHERE`。

### 6.3 右側面板「執行作業」→ 加寫入模式與 merge 分支

```
目標資料表 [TARGET].[dbo.ExternalIdentityMappings]   別名 [mapping]
寫入模式  ( ) 直接新增 ADD     (•) 合併 / Upsert（依比對鍵）

  ▸ 比對鍵（ON）：mapping.[ExternalSystemType] == N'LDAP'
                && mapping.[ExternalId] == account.[LdapId]   [＋ 新增條件]

  ▸ 未命中時（NOT MATCHED）  [ADD ▾]   → 下方欄位對應表
  ▸ 命中時  （MATCHED）       [SKIP ▾]  （SKIP / UPDATE …）
```

- 新增「**寫入模式**」切換：`直接新增 ADD` ↔ `合併 Upsert`。
- 選「合併」時展開「**比對鍵（ON）**」「**MATCHED**」「**NOT MATCHED**」三塊；目標表需取別名（DSL 端為 `mapping = TARGET.[…]`，GUI 可自動生成或讓使用者命名）。
- **現有那張欄位對應表，歸屬到「NOT MATCHED → ADD」**；若 MATCHED 選 UPDATE，再開一張平行的 UPDATE 欄位表。

### 6.4 「值來源」下拉改為 `別名.欄位` 兩級選擇
現況選項：生成 / 比對表欄位 / 來源欄位 / 常值 / 合成欄位。多別名後「來源欄位」「比對表欄位」應合併為**「欄位」**，先選別名（`entra` / `account` / `mapping`）再選欄位，直接對應 DSL 的 `account.[Id]` / `entra.[id]`。建議收斂為：**產生器(Gen.*) / 欄位(別名.欄位) / 常值 / 合成欄位(模板)**。

### 6.5 「啟用比對」checkbox 語意修正（重要）
舊 checkbox「啟用比對（IF 命中才寫入）」現在身兼三義，需拆解：
- 「有沒有查表」→ 由 §6.2 關聯表清單的有無決定。
- 「查表未命中怎麼辦」→ 移到**每張 JOIN 的 inner/left 政策**。
- 「目標已存在才寫 / 不存在才寫」→ 是 §6.3 的 merge 模式，與查表無關。

此 checkbox 應**移除**，語意散到上述三處。

### 6.6 合成欄位（模板）編輯器
- 模板插入助手應插入**別名限定**的欄位（`{account.[DisplayName]}`，不是 `{[dbo].[DirectoryAccounts].[DisplayName]}`）。
- 套用 §7 的新高亮（插值洞內欄位會單獨上色）。

### 6.7 雙向同步
上述每個新 UI 區塊都要能序列化回 DSL 子句（FROM / JOIN 清單 / WHERE / INTO（含別名）/ ON / MATCHED / NOT MATCHED），並反向把 DSL 解析回 UI。沿用現有正規化哲學：DSL 端的舊式/相容寫法解析後一律以新 AST 呈現在 GUI。

---

## 7. CodeMirror 高亮（StreamLanguage，含字串插值）

相對 v0.1 的三點變化：**(a) 字串改為巢狀狀態解析**，進入 `N'…'` 後遇 `{` 切到插值狀態，把洞內 `account.[DisplayName]` / `Gen.ULID` 當欄位/產生器上色，`}` 切回；`{{`/`}}` 維持字面。**(b) 新增裸識別字上色**（別名/成員）。**(c) `bool`/`null`/`punctuation` 經 `tokenTable` 明確對應 tag**（legacy mode 預設不一定支援這幾個名稱）。

```typescript
// ETL DSL 語法高亮（含字串插值）
import { StreamLanguage } from "@codemirror/language";
import { tags as t } from "@lezer/highlight";

type ETLState = {
  inBlockComment: boolean;
  inString: boolean;   // 是否在字串字面值內
  inInterp: boolean;   // 是否在字串內的 {…} 插值洞內
};

const KEYWORDS_SINGLE =
  /^(WORK|FROM|JOIN|WHERE|INTO|ON|MATCHED|MATCH|ADD|UPDATE|SKIP|DELETE|GO|SOURCE|TARGET|CONNECTION|FILE|TYPE|PATH|SHEET|ENCODING|HEADER|TABLE|QUERY|IS|EMPTY|NOT|IN|LIKE|BETWEEN|JUDGE|EXECUTE|IF)\b/i;

const etlLanguage = StreamLanguage.define<ETLState>({
  startState: () => ({ inBlockComment: false, inString: false, inInterp: false }),

  token(stream, state) {
    // 0. 多行區塊註解 /// … ///（會跨行，必須最先處理）
    if (state.inBlockComment) {
      if (stream.match(/^.*?\/\/\//)) state.inBlockComment = false;
      else stream.skipToEnd();
      return "comment";
    }

    // 字串以單行為界：跨行未閉合則於行首重置，避免污染後續行
    if (stream.sol() && state.inString) {
      state.inString = false;
      state.inInterp = false;
    }

    // 1. 字串內部（含 {…} 插值）
    if (state.inString) {
      // 1a. 插值洞內：與洞外一般語法相同的上色
      if (state.inInterp) {
        if (stream.match(/^\}/)) { state.inInterp = false; return "punctuation"; }
        if (stream.match(/^Gen\.\w+(\s*\(\s*Text\s*\))?/i)) return "keyword";
        if (stream.match(/^\[[^\]\n]*\]/)) return "variableName";
        if (stream.match(/^[A-Za-z_]\w*/)) return "propertyName";
        if (stream.match(/^\d+(\.\d+)?/)) return "number";
        if (stream.match(/^(==|!=|<=|>=|&&|\|\||[=!<>])/)) return "operator";
        if (stream.match(/^[.,]/)) return "punctuation";
        stream.next();
        return null;
      }
      // 1b. 字串本體（插值洞外）
      if (stream.match(/^''/)) return "string";                          // '' 跳脫單引號（須先於單一 ' 檢查）
      if (stream.match(/^'/)) { state.inString = false; return "string"; } // 結束字串
      if (stream.match(/^(\{\{|\}\})/)) return "string";                 // {{ }} 字面大括號
      if (stream.match(/^\{/)) { state.inInterp = true; return "punctuation"; } // 進入插值洞
      if (stream.match(/^[^'{}]+/)) return "string";                     // 一般字串字元
      stream.next();
      return "string";
    }

    // 2. 單行註解
    if (stream.match(/^(--|\/\/).*/)) return "comment";

    // 3. 區塊註解起始
    if (stream.match(/^\/\/\//)) {
      if (!stream.match(/^.*?\/\/\//)) {     // 同行未收尾 → 進入跨行
        stream.skipToEnd();
        state.inBlockComment = true;
      }
      return "comment";
    }

    // 4. 字串起始 N'…' / '…'
    if (stream.match(/^N?'/i)) { state.inString = true; return "string"; }

    // 5. 字面值
    if (stream.match(/^\b(TRUE|FALSE)\b/i)) return "bool";
    if (stream.match(/^\bNULL\b/i)) return "null";
    if (stream.match(/^\d+(\.\d+)?/)) return "number";

    // 6. 產生器 Gen.*
    if (stream.match(/^Gen\.\w+(\s*\(\s*Text\s*\))?/i)) return "keyword";

    // 7. 多字關鍵字（必須排在單字之前）
    if (stream.match(/^IS\s+NOT\s+EMPTY\b/i)) return "keyword";
    if (stream.match(/^IS\s+EMPTY\b/i)) return "keyword";
    if (stream.match(/^NOT\s+MATCHED\b/i)) return "keyword";

    // 8. 單字關鍵字
    if (stream.match(KEYWORDS_SINGLE)) return "keyword";

    // 9. 中括號識別字 [dbo].[Table]
    if (stream.match(/^\[[^\]\n]*\]/)) return "variableName";

    // 10. 一般識別字 / 別名 / 成員（account、mapping、entra…）
    if (stream.match(/^[A-Za-z_]\w*/)) return "propertyName";

    // 11. 運算子（多字元優先）
    if (stream.match(/^(==|!=|<=|>=|&&|\|\||[=!<>])/)) return "operator";

    // 12. 標點 / 結構
    if (stream.match(/^[{}()\[\],.]/)) return "punctuation";

    stream.next();
    return null;
  },

  // legacy mode 對非預設 tag 名稱的明確對應；若你現有設定已能顯示這些顏色可省略
  tokenTable: {
    bool: t.bool,
    null: t.null,
    punctuation: t.punctuation,
  },
});

export { etlLanguage };
```

驗證範例 `N'MICROSOFT_ENTRA_ID: {account.[DisplayName]}'` 的切詞：`N'` 與 `MICROSOFT_ENTRA_ID: ` → string；`{` → punctuation（進洞）；`account` → propertyName；`.` → punctuation；`[DisplayName]` → variableName；`}` → punctuation（出洞）；`'` → string。`{{literal}}` 維持 string。

> 若要讓插值洞的 `{` `}` 與一般標點不同色以更顯眼，可在 1a/1b 把它們改回傳獨立 tag（如 `"brace"`）並在 `tokenTable` 補對應；非必要。

---

## 8. 漸進落地建議（不必一次做完）

1. **Phase 1（DSL 核心）**：parser/AST 支援 `FROM` / `JOIN … ON` / `WHERE` / `INTO`（含別名）/ `ON` / `MATCHED` / `NOT MATCHED`、`Field` 改帶 alias；保留舊式相容解析並正規化；套用 §7 高亮。executor 加 WHERE 過濾、多 JOIN、merge 的鍵集合 probe（§5.2）。
2. **Phase 2（GUI）**：中間面板拆「來源與關聯」+「篩選」（§6.2）；右側加「寫入模式 + ON + MATCHED/NOT MATCHED」（§6.3）；值來源改 `別名.欄位`（§6.4）；移除舊 checkbox（§6.5）；雙向同步補齊（§6.7）。
3. **Phase 3（強化）**：JOIN 的 left/inner 政策 UI、`MATCHED → UPDATE` 分支、來源端去重（§5.3）、`DELETE`。

---

© Elu Technology Ltd. — 內部開發規格
