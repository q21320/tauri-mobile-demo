use axum::{routing::get, Router, serve};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use std::sync::Mutex;
use std::path::PathBuf;

// 存储 PDF 数据的全局状态
struct PdfState {
    data: Option<Vec<u8>>,
}

fn init_rustls_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
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

async fn start_http_server(app_handle: tauri::AppHandle) {
    let app = Router::new()
        .route("/haippt/api/v1/show", get(get_page_handler))
        .with_state(app_handle);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("HTTP server listening on http://{}", addr);

    if let Err(e) = serve(listener, app.into_make_service()).await {
        eprintln!("HTTP server error: {}", e);
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

        let url = format!(
            "wss://robot.haihuman.com/haicommand/api/v2/iotSocket?did={}&name={}&tempId=Tiot26091110353mtf",
            device_id, device_name
        );
        println!("[ws] 连接 WebSocket: {}", url);

        let connect_result = tokio_tungstenite::connect_async(&url).await;
        let (ws_stream, _) = match connect_result {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[ws] 连接失败: {}, 5秒后重试", e);
                let _ = app_handle.emit("ws_status", &format!("连接失败: {}", e));
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        println!("[ws] WebSocket 已连接");
        let _ = app_handle.emit("ws_status", "已连接");

        let (mut write, mut read) = ws_stream.split();

        // 心跳: 每30秒发送 "1"
        let heartbeat = async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                if write.send(tokio_tungstenite::tungstenite::Message::Text("1".into())).await.is_err() {
                    println!("[ws] 心跳发送失败");
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
                        println!("[ws] 收到消息: {}", text);
                        handle_ws_message(&msg_handle, &text).await;
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                        println!("[ws] 服务端关闭连接");
                        break;
                    }
                    Err(e) => {
                        eprintln!("[ws] 读取错误: {}", e);
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

        println!("[ws] 连接断开，5秒后重连...");
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
            eprintln!("[ws] 消息解析失败: {}", e);
            return;
        }
    };

    match msg.msg_type.as_str() {
        "data" => {
            println!("[ws] 收到 data 消息: {}", msg.data);
            handle_ws_data(app_handle, &msg.data).await;
        }
        "show" => {
            println!("[ws] 收到 show 消息: {}", msg.data);
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
                    println!("[ws] show 加载文件: {}", path.display());
                    match std::fs::read(&path) {
                        Ok(bytes) => {
                            let len = bytes.len();
                            let fname = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                            let state = app_handle.state::<Mutex<PdfState>>();
                            let mut state = state.lock().unwrap();
                            state.data = Some(bytes);
                            println!("[ws] PdfState 已更新: {} ({} bytes)", fname, len);
                        }
                        Err(e) => {
                            eprintln!("[ws] 读取文件失败: {}", e);
                        }
                    }
                } else {
                    println!("[ws] show: 未找到文件 {:?}", show_req.file_name);
                }

                // 发送事件通知前端加载指定页
                let _ = app_handle.emit("pdf_show", &serde_json::json!({
                    "fileName": show_req.file_name,
                    "page": show_req.page
                }));
            }
        }
        other => {
            println!("[ws] 未知消息类型: {}", other);
        }
    }
}

// 处理 data 消息：下载文件到 Android files 目录
async fn handle_ws_data(app_handle: &tauri::AppHandle, data_json: &str) {
    println!("[ws] handle_ws_data 开始, data长度: {}", data_json.len());

    let files: Vec<RemoteFile> = match serde_json::from_str::<Vec<RemoteFile>>(data_json) {
        Ok(f) => {
            println!("[ws] 解析到 {} 个文件", f.len());
            f
        }
        Err(e) => {
            eprintln!("[ws] data 消息解析失败: {}", e);
            eprintln!("[ws] 原始数据: {}", data_json);
            return;
        }
    };

    if files.is_empty() {
        println!("[ws] 文件列表为空，跳过");
        return;
    }

    // Android files 目录（与 PDF 扫描目录一致）
    let files_dir = std::path::Path::new(
        "/storage/emulated/0/Android/data/com.pdf_link_demo.app/files"
    );
    let _ = std::fs::create_dir_all(files_dir);
    println!("[ws] files 目录: {}", files_dir.display());

    let client = reqwest::Client::new();
    let mut downloaded = 0;

    for file in &files {
        let dst = files_dir.join(&file.name);

        // 始终重新下载，确保修改时间最新（rescan 按修改时间排序）
        println!("[ws] 下载文件: {} -> {}", file.url, dst.display());
        match client.get(&file.url).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    match resp.bytes().await {
                        Ok(bytes) => {
                            if let Err(e) = std::fs::write(&dst, &bytes) {
                                eprintln!("[ws] 写入文件失败 [{}]: {}", file.name, e);
                            } else {
                                println!("[ws] 下载成功: {} ({} bytes)", file.name, bytes.len());
                                downloaded += 1;
                            }
                        }
                        Err(e) => {
                            eprintln!("[ws] 读取响应失败 [{}]: {}", file.name, e);
                        }
                    }
                } else {
                    eprintln!("[ws] HTTP {} for {}", resp.status(), file.url);
                }
            }
            Err(e) => {
                eprintln!("[ws] 下载失败 [{}]: {}", file.name, e);
            }
        }
    }

    println!("[ws] 数据同步完成，共 {} 个文件", downloaded);

    // 重新扫描目录并加载 PDF
    println!("[ws] 开始 rescan_and_load_pdf...");
    rescan_and_load_pdf(app_handle);
    println!("[ws] rescan_and_load_pdf 完成");

    println!("[ws] 发送 ws_data_synced 事件");
    let _ = app_handle.emit("ws_data_synced", downloaded);
}

// 重新扫描 files 目录，加载第一个 PDF 到 PdfState
fn rescan_and_load_pdf(app_handle: &tauri::AppHandle) {
    let dir = std::path::Path::new(
        "/storage/emulated/0/Android/data/com.pdf_link_demo.app/files"
    );
    if !dir.exists() {
        println!("[pdf_rescan] 目录不存在");
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
                    println!("[pdf_rescan] 找到最新 PDF: {}", path.display());
                    match std::fs::read(&path) {
                        Ok(bytes) => {
                            println!("[pdf_rescan] 读取成功, 大小: {} bytes", bytes.len());
                            let state = app_handle.state::<Mutex<PdfState>>();
                            let mut state = state.lock().unwrap();
                            state.data = Some(bytes);
                            println!("[pdf_rescan] PdfState 已更新");
                        }
                        Err(e) => {
                            eprintln!("[pdf_rescan] 读取文件失败: {}", e);
                        }
                    }
                }
                None => {
                    println!("[pdf_rescan] 目录下没有 PDF 文件");
                }
            }
        }
        Err(e) => {
            eprintln!("[pdf_rescan] 读取目录失败: {}", e);
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

// Tauri command: 前端调用获取 PDF 数据
#[tauri::command]
fn get_pdf_data(state: tauri::State<'_, Mutex<PdfState>>) -> Option<Vec<u8>> {
    let state = state.lock().unwrap();
    state.data.clone()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_rustls_crypto_provider();
    
    // 初始化 PDF 状态
    let pdf_state = Mutex::new(PdfState { data: None });
    
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .manage(pdf_state)
        .invoke_handler(tauri::generate_handler![get_pdf_data, get_device_info, save_device_name])
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
                        println!("[pdf_scan] 目录不存在: {}", dir.display());
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
                                    println!("[pdf_scan] 找到 PDF: {}", path);
                                    match std::fs::read(&path) {
                                        Ok(bytes) => {
                                            println!("[pdf_scan] 读取成功, 大小: {} bytes", bytes.len());
                                            // 存储到状态中
                                            let mut state = state.lock().unwrap();
                                            state.data = Some(bytes);
                                            println!("[pdf_scan] 数据已存储，等待前端请求");
                                        }
                                        Err(e) => {
                                            println!("[pdf_scan] 读取文件失败: {}", e);
                                        }
                                    }
                                }
                                None => {
                                    println!("[pdf_scan] 目录下没有 PDF 文件");
                                }
                            }
                        }
                        Err(e) => {
                            println!("[pdf_scan] 读取目录失败: {}", e);
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
