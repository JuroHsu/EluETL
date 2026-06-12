use tracing_subscriber::EnvFilter;

/// 初始化結構化日誌。
///
/// 目前輸出到 stderr；Week 2 加上 tracing-appender 滾動檔案
/// （`{appLogDir}`，保留 30 天）與 JSON 格式審計事件分流。
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
