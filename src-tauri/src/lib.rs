use axum::{routing::get, Router, serve};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use std::sync::Mutex;
use std::path::PathBuf;
use std::sync::OnceLock;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

// 日志文件全局路径（Android: 应用外部 files 目录，adb pull 可直接取；桌面: 当前目录 logs/）
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

fn log_init() {
    let dir = if cfg!(target_os = "android") {
        PathBuf::from("/storage/emulated/0/Android/data/com.pdf_link_demo.app/files/logs")
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("logs")
    };
    let _ = std::fs::create_dir_all(&dir);
    let _ = LOG_PATH.set(dir.join("app.txt"));
    log_write("I", "=== 日志会话开始 ===");
}

// Unix 时间戳转 UTC 可读时间（不引入第三方时间库）
fn now_stamp() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let (days, secs) = (dur.as_secs() / 86400, dur.as_secs() % 86400);
    // civil_from_days 算法（Howard Hinnant）
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        y, m, d,
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        dur.subsec_millis()
    )
}

// 写一行日志: 同时输出到文件与 stdout（Android 上 stdout 进 logcat）
fn log_write(level: &str, msg: &str) {
    let line = format!("[{}][{}] {}", now_stamp(), level, msg);
    {
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), "{}", line);
    }
    if let Some(path) = LOG_PATH.get() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            use std::io::Write;
            if writeln!(f, "{}", line).is_ok() {
                let _ = f.flush();
                let _ = f.sync_all();
            }
        }
    }
}

macro_rules! log_info {
    ($($arg:tt)*) => {
        log_write("I", &format!($($arg)*))
    };
}

macro_rules! log_error {
    ($($arg:tt)*) => {
        log_write("E", &format!($($arg)*))
    };
}

// 存储 PDF 文件路径的全局状态
struct PdfState {
    path: Option<String>,
}

fn init_rustls_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// 内网服务器使用自签名证书: 自定义校验器跳过证书链校验（仅演示环境使用）
#[derive(Debug)]
struct SkipCertVerifier;

impl rustls::client::danger::ServerCertVerifier for SkipCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// 构建允许自签名证书的 wss 连接器
fn self_signed_connector() -> tokio_tungstenite::Connector {
    let tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(SkipCertVerifier))
        .with_no_client_auth();
    tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(tls_config))
}

#[derive(Debug, Deserialize)]
struct PageParams {
    page: u32,
}

#[derive(Debug, Serialize)]
struct PageResult {
    page: u32,
}

async fn get_page_handler(
    axum::extract::State(app_handle): axum::extract::State<tauri::AppHandle>,
    params: axum::extract::Query<PageParams>,
) -> axum::Json<PageResult> {
    let _ = app_handle.emit("page_change", params.page);
    axum::Json(PageResult { page: params.page })
}

// PDF 文件服务端：从 PdfState 读取当前 PDF 并返回原始字节
async fn get_pdf_handler(
    axum::extract::State(app_handle): axum::extract::State<tauri::AppHandle>,
) -> impl axum::response::IntoResponse {
    let state = app_handle.state::<Mutex<PdfState>>();
    let state = state.lock().unwrap();
    let path = match &state.path {
        Some(p) => p.clone(),
        None => {
            return axum::response::Response::builder()
                .status(404)
                .body(axum::body::Body::from("No PDF loaded"))
                .unwrap();
        }
    };
    drop(state);
    
    match std::fs::read(&path) {
        Ok(bytes) => {
            log_info!("[http] 提供 PDF: {} ({} bytes)", path, bytes.len());
            axum::response::Response::builder()
                .status(200)
                .header("Content-Type", "application/pdf")
                .header("Content-Length", bytes.len())
                .body(axum::body::Body::from(bytes))
                .unwrap()
        }
        Err(e) => {
            log_error!("[http] 读取 PDF 失败: {}", e);
            axum::response::Response::builder()
                .status(500)
                .body(axum::body::Body::from(format!("Read error: {}", e)))
                .unwrap()
        }
    }
}

async fn start_http_server(app_handle: tauri::AppHandle) {
    let app = Router::new()
        .route("/haippt/api/v1/show", get(get_page_handler))
        .route("/pdf", get(get_pdf_handler))
        .with_state(app_handle);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    log_info!("HTTP server listening on http://{}", addr);

    if let Err(e) = serve(listener, app.into_make_service()).await {
        log_error!("HTTP server error: {}", e);
    }
}

// 设备信息结构
#[derive(Debug, Serialize, Deserialize, Clone)]
struct DeviceInfo {
    device_id: String,
    device_name: String,
    attachment_info: String,
}

// WebSocket 消息结构
#[derive(Debug, Deserialize)]
struct SocketMsg {
    #[serde(rename = "mt")]
    msg_type: String,
    #[serde(rename = "did")]
    device_id: String,
    data: String,
}

// 获取或创建设备ID
fn get_or_create_device_id(config_dir: &PathBuf) -> String {
    let id_file = config_dir.join("device_id.txt");
    std::fs::read_to_string(&id_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let new_id = uuid::Uuid::new_v4().to_string();
            let _ = std::fs::write(&id_file, &new_id);
            new_id
        })
}

// 获取设备名称
fn get_device_name(config_dir: &PathBuf) -> String {
    let name_file = config_dir.join("device_name.txt");
    std::fs::read_to_string(&name_file)
        .unwrap_or_default()
        .trim()
        .to_string()
}

// WebSocket 连接（含心跳 + 消息处理 + 断线重连）
async fn start_websocket(app_handle: tauri::AppHandle) {
    loop {
        let config_dir = app_handle.path().app_config_dir().unwrap_or_else(|_| PathBuf::from("."));
        let _ = std::fs::create_dir_all(&config_dir);
        let device_id = get_or_create_device_id(&config_dir);
        let device_name = get_device_name(&config_dir);
        // URL 编码设备名称（前端可能写入中文，需要 percent-encoding）
        let device_name_encoded = utf8_percent_encode(&device_name, NON_ALPHANUMERIC).to_string();

        let url = format!(
            "wss://10.11.235.174:8805/haicommand/api/v2/iotSocket?did={}&name={}&tempId=Tiot2609201013pldt",
            device_id, device_name_encoded
        );
        log_info!("[ws] 连接 WebSocket: {}", url);

        let connect_result = tokio_tungstenite::connect_async_tls_with_config(
            &url,
            None,
            false,
            Some(self_signed_connector()),
        )
        .await;
        let (ws_stream, _) = match connect_result {
            Ok(r) => r,
            Err(e) => {
                log_error!("[ws] 连接失败: {}, 5秒后重试", e);
                let _ = app_handle.emit("ws_status", &format!("连接失败: {}", e));
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        log_info!("[ws] WebSocket 已连接");
        let _ = app_handle.emit("ws_status", "已连接");

        let (mut write, mut read) = ws_stream.split();

        // 心跳: 每30秒发送 "1"
        let heartbeat = async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                if write.send(tokio_tungstenite::tungstenite::Message::Text("1".into())).await.is_err() {
                    log_info!("[ws] 心跳发送失败");
                    break;
                }
            }
        };

        // 接收消息
        let msg_handle = app_handle.clone();
        let msg_handler = async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                        if text == "1" {
                            continue; // 心跳回应，忽略
                        }
                        log_info!("[ws] 收到消息: {}", text);
                        handle_ws_message(&msg_handle, &text).await;
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                        log_info!("[ws] 服务端关闭连接");
                        break;
                    }
                    Err(e) => {
                        log_error!("[ws] 读取错误: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        };

        // 等待心跳或消息任一结束（连接断开）
        tokio::select! {
            _ = heartbeat => {},
            _ = msg_handler => {},
        }

        log_info!("[ws] 连接断开，5秒后重连...");
        let _ = app_handle.emit("ws_status", "已断开，重连中...");
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }
}

// 远程文件结构（data 消息中的文件对象）
#[derive(Debug, Deserialize)]
struct RemoteFile {
    name: String,
    url: String,
    format: String,
}

// 处理 WebSocket 消息
async fn handle_ws_message(app_handle: &tauri::AppHandle, text: &str) {
    let msg: SocketMsg = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            log_error!("[ws] 消息解析失败: {}", e);
            return;
        }
    };

    match msg.msg_type.as_str() {
        "data" => {
            log_info!("[ws] 收到 data 消息: {}", msg.data);
            handle_ws_data(app_handle, &msg.data).await;
        }
        "show" => {
            log_info!("[ws] 收到 show 消息: {}", msg.data);
            if let Ok(show_req) = serde_json::from_str::<ShowReq>(&msg.data) {
                // 根据 fileName 查找并加载 PDF
                let files_dir = std::path::Path::new(
                    "/storage/emulated/0/Android/data/com.pdf_link_demo.app/files"
                );
                let target_path = if let Some(ref name) = show_req.file_name {
                    // 在目录中查找匹配的文件名
                    let matched = std::fs::read_dir(files_dir)
                        .ok()
                        .and_then(|entries| {
                            entries.filter_map(|e| e.ok()).find(|e| {
                                e.file_name().to_string_lossy() == name.as_str()
                            })
                        });
                    matched.map(|e| e.path())
                } else {
                    None
                };

                // 如果找到文件，加载到 PdfState
                if let Some(path) = target_path {
                    log_info!("[ws] show 加载文件: {}", path.display());
                    match std::fs::read(&path) {
                        Ok(bytes) => {
                            let len = bytes.len();
                            let fname = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                            let state = app_handle.state::<Mutex<PdfState>>();
                            let mut state = state.lock().unwrap();
                            state.path = Some(path.to_string_lossy().to_string());
                            log_info!("[ws] PdfState 已更新: {} ({} bytes)", fname, len);
                        }
                        Err(e) => {
                            log_error!("[ws] 读取文件失败: {}", e);
                        }
                    }
                } else {
                    log_info!("[ws] show: 未找到文件 {:?}", show_req.file_name);
                }

                // 发送事件通知前端加载指定页
                let _ = app_handle.emit("pdf_show", &serde_json::json!({
                    "fileName": show_req.file_name,
                    "page": show_req.page
                }));
            }
        }
        other => {
            log_info!("[ws] 未知消息类型: {}", other);
        }
    }
}

// 处理 data 消息：下载文件到 Android files 目录
async fn handle_ws_data(app_handle: &tauri::AppHandle, data_json: &str) {
    log_info!("[ws] handle_ws_data 开始, data长度: {}", data_json.len());

    let files: Vec<RemoteFile> = match serde_json::from_str::<Vec<RemoteFile>>(data_json) {
        Ok(f) => {
            log_info!("[ws] 解析到 {} 个文件", f.len());
            f
        }
        Err(e) => {
            log_error!("[ws] data 消息解析失败: {}", e);
            log_error!("[ws] 原始数据: {}", data_json);
            return;
        }
    };

    if files.is_empty() {
        log_info!("[ws] 文件列表为空，跳过");
        return;
    }

    // Android files 目录（与 PDF 扫描目录一致）
    let files_dir = std::path::Path::new(
        "/storage/emulated/0/Android/data/com.pdf_link_demo.app/files"
    );
    let _ = std::fs::create_dir_all(files_dir);
    log_info!("[ws] files 目录: {}", files_dir.display());

    // 内网服务器为自签名证书，允许无效证书下载
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let mut downloaded = 0;

    for file in &files {
        let dst = files_dir.join(&file.name);

        // 检查本地文件是否已存在
        if dst.exists() {
            log_info!("[ws] 文件已存在，跳过下载: {} (更新修改时间)", file.name);
            // 更新修改时间，确保 rescan 按时间排序时能识别为最新
            let now = std::time::SystemTime::now();
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&dst) {
                let _ = f.set_modified(now);
            }
            downloaded += 1;
            continue;
        }

        // 不存在才下载
        log_info!("[ws] 下载文件: {} -> {}", file.url, dst.display());
        match client.get(&file.url).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    match resp.bytes().await {
                        Ok(bytes) => {
                            if let Err(e) = std::fs::write(&dst, &bytes) {
                                log_error!("[ws] 写入文件失败 [{}]: {}", file.name, e);
                            } else {
                                log_info!("[ws] 下载成功: {} ({} bytes)", file.name, bytes.len());
                                downloaded += 1;
                            }
                        }
                        Err(e) => {
                            log_error!("[ws] 读取响应失败 [{}]: {}", file.name, e);
                        }
                    }
                } else {
                    log_error!("[ws] HTTP {} for {}", resp.status(), file.url);
                }
            }
            Err(e) => {
                log_error!("[ws] 下载失败 [{}]: {}", file.name, e);
            }
        }
    }

    log_info!("[ws] 数据同步完成，共 {} 个文件", downloaded);

    // 重新扫描目录并加载 PDF
    log_info!("[ws] 开始 rescan_and_load_pdf...");
    rescan_and_load_pdf(app_handle);
    log_info!("[ws] rescan_and_load_pdf 完成");

    log_info!("[ws] 发送 ws_data_synced 事件");
    let _ = app_handle.emit("ws_data_synced", downloaded);
}

// 重新扫描 files 目录，加载第一个 PDF 到 PdfState
fn rescan_and_load_pdf(app_handle: &tauri::AppHandle) {
    let dir = std::path::Path::new(
        "/storage/emulated/0/Android/data/com.pdf_link_demo.app/files"
    );
    if !dir.exists() {
        log_info!("[pdf_rescan] 目录不存在");
        return;
    }
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            // 收集所有 PDF 文件，按修改时间降序排列（最新的在前）
            let mut pdfs: Vec<(std::path::PathBuf
                , std::time::SystemTime)> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .map(|ext| ext.eq_ignore_ascii_case("pdf"))
                        .unwrap_or(false)
                })
                .filter_map(|p| {
                    p.metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .map(|t| (p, t))
                })
                .collect();
            pdfs.sort_by(|a, b| b.1.cmp(&a.1)); // 最新的在前

            match pdfs.first() {
                Some((path, _)) => {
                    log_info!("[pdf_rescan] 找到最新 PDF: {}", path.display());
                    match std::fs::read(&path) {
                        Ok(bytes) => {
                            log_info!("[pdf_rescan] 读取成功, 大小: {} bytes", bytes.len());
                            let state = app_handle.state::<Mutex<PdfState>>();
                            let mut state = state.lock().unwrap();
                            state.path = Some(path.to_string_lossy().to_string());
                            log_info!("[pdf_rescan] PdfState 已更新");
                        }
                        Err(e) => {
                            log_error!("[pdf_rescan] 读取文件失败: {}", e);
                        }
                    }
                }
                None => {
                    log_info!("[pdf_rescan] 目录下没有 PDF 文件");
                }
            }
        }
        Err(e) => {
            log_error!("[pdf_rescan] 读取目录失败: {}", e);
        }
    }
}

#[derive(Debug, Deserialize)]
struct ShowReq {
    #[serde(rename = "fileName")]
    file_name: Option<String>,
    page: u32,
}

// Tauri command: 获取设备信息
#[tauri::command]
fn get_device_info(app: tauri::AppHandle) -> Result<DeviceInfo, String> {
    // 获取设备ID（首次生成 UUID 并持久化，后续复用）
    let config_dir = app.path().app_config_dir().unwrap_or_else(|_| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&config_dir);
    let id_file = config_dir.join("device_id.txt");
    let device_id = std::fs::read_to_string(&id_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let new_id = uuid::Uuid::new_v4().to_string();
            let _ = std::fs::write(&id_file, &new_id);
            new_id
        });
    let name_file = config_dir.join("device_name.txt");
    let device_name = std::fs::read_to_string(&name_file)
        .unwrap_or_default()
        .trim()
        .to_string();

    // 附件信息（示例数据，可根据实际业务替换）
    let attachment_info = "PDF联动演示附件-001".to_string();

    Ok(DeviceInfo {
        device_id,
        device_name,
        attachment_info,
    })
}

// Tauri command: 保存设备名称
#[tauri::command]
fn save_device_name(app: tauri::AppHandle, name: String) -> Result<(), String> {
    let config_dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    let name_file = config_dir.join("device_name.txt");
    std::fs::write(&name_file, &name).map_err(|e| e.to_string())?;
    Ok(())
}

// Tauri command: 前端 console/异常上报，写入日志文件
#[tauri::command]
fn write_log(level: String, msg: String) {
    log_write(&level, &msg);
}

// PDF 数据返回结构（包含文件名和 base64 数据）
#[derive(serde::Serialize)]
struct PdfData {
    file_name: String,
    base64: String,
}

// Tauri command: 前端调用获取 PDF 数据（base64 编码 + 文件名）
#[tauri::command]
fn get_pdf_data(state: tauri::State<'_, Mutex<PdfState>>) -> Option<PdfData> {
    let state = state.lock().unwrap();
    let path_str = state.path.as_ref()?;
    let path = std::path::Path::new(path_str);
    let file_name = path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    match std::fs::read(path_str) {
        Ok(bytes) => {
            log_info!("[pdf] 读取文件 {} ({} bytes), 转 base64", path_str, bytes.len());
            Some(PdfData {
                file_name,
                base64: BASE64.encode(&bytes),
            })
        }
        Err(e) => {
            log_error!("[pdf] 读取文件失败: {}", e);
            None
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    log_init();
    log_info!("App 启动, os={}, pkg version={}", std::env::consts::OS, env!("CARGO_PKG_VERSION"));
    init_rustls_crypto_provider();
    
    // 初始化 PDF 状态
    let pdf_state = Mutex::new(PdfState { path: None });
    
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .manage(pdf_state)
        .invoke_handler(tauri::generate_handler![get_pdf_data, get_device_info, save_device_name, write_log])
        .setup(|app| {
            // Android: 启动时扫描 files 目录，读取第一个 PDF 文件内容
            #[cfg(target_os = "android")]
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let state = handle.state::<Mutex<PdfState>>();
                    let dir = std::path::Path::new(
                        "/storage/emulated/0/Android/data/com.pdf_link_demo.app/files"
                    );
                    if !dir.exists() {
                        log_info!("[pdf_scan] 目录不存在: {}", dir.display());
                        return;
                    }
                    match std::fs::read_dir(dir) {
                        Ok(entries) => {
                            let pdf: Option<String> = entries
                                .filter_map(|e| e.ok())
                                .map(|e| e.path())
                                .filter(|p| {
                                    p.extension()
                                        .map(|ext| ext.eq_ignore_ascii_case("pdf"))
                                        .unwrap_or(false)
                                })
                                .next()
                                .map(|p| p.to_string_lossy().to_string());

                            match pdf {
                                Some(path) => {
                                    log_info!("[pdf_scan] 找到 PDF: {}", path);
                                    match std::fs::read(&path) {
                                        Ok(bytes) => {
                                            log_info!("[pdf_scan] 读取成功, 大小: {} bytes", bytes.len());
                                            let fname = std::path::Path::new(&path)
                                                .file_name()
                                                .unwrap_or_default()
                                                .to_string_lossy()
                                                .to_string();
                                            // 存储文件路径
                                            let mut state = state.lock().unwrap();
                                            state.path = Some(path.clone());
                                            log_info!("[pdf_scan] 路径已存储: {}", path);
                                            log_info!("[pdf_scan] 数据已存储，等待前端请求");
                                            drop(state); // 释放锁，避免 emit 时死锁
                                            // 启动时自动显示第一个 PDF 的第一页
                                            let _ = handle.emit("pdf_show", &serde_json::json!({
                                                "fileName": fname,
                                                "page": 1
                                            }));
                                            log_info!("[pdf_scan] 已发送 pdf_show 事件: {} 第 1 页", fname);
                                        }
                                        Err(e) => {
                                            log_info!("[pdf_scan] 读取文件失败: {}", e);
                                        }
                                    }
                                }
                                None => {
                                    log_info!("[pdf_scan] 目录下没有 PDF 文件");
                                }
                            }
                        }
                        Err(e) => {
                            log_info!("[pdf_scan] 读取目录失败: {}", e);
                        }
                    }
                });
            }

            // 启动 WebSocket 连接（心跳 + 消息接收 + 断线重连）
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                tokio::runtime::Runtime::new().unwrap().block_on(start_websocket(handle));
            });

            let handle = app.handle().clone();
            std::thread::spawn(move || {
                tokio::runtime::Runtime::new().unwrap().block_on(start_http_server(handle));
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
