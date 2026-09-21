/**
 * 前端日志上报: 将 console 输出与全局异常转发到 Rust 端 write_log 命令,
 * 由 Rust 写入设备日志文件（Android: files/logs/app.txt）。
 * 必须在 polyfills 之后、App 之前导入, 保证尽早挂钩。
 *
 * 启动早期 IPC 未就绪时, 日志先缓存在内存, 就绪后自动补发。
 */
import { invoke } from "@tauri-apps/api/core";

// 启动早期 IPC 未就绪, 先缓存日志
let _ipcReady = false;
const _logBuffer = [];

function formatArg(a) {
  if (typeof a === "string") return a;
  if (a instanceof Error) return `${a.name}: ${a.message} | stack: ${a.stack || ""}`;
  try {
    return JSON.stringify(a);
  } catch {
    return String(a);
  }
}

function sendLog(level, args) {
  const msg = args.map(formatArg).join(" ");
  if (!_ipcReady) {
    _logBuffer.push({ level, msg });
    return;
  }
  try {
    invoke("write_log", { level, msg }).catch(() => {});
  } catch {
    // 极端情况 IPC 再次断开, 重新缓冲
    _ipcReady = false;
    _logBuffer.push({ level, msg });
  }
}

// 尝试刷新缓冲区: 发送一条探测日志, 成功则标记 IPC 就绪并补发所有缓存
function tryFlushBuffer() {
  if (_ipcReady || _logBuffer.length === 0) return;
  invoke("write_log", { level: "I", msg: "[logger] IPC 已就绪, 补发 " + _logBuffer.length + " 条缓存日志" })
    .then(() => {
      _ipcReady = true;
      const batch = _logBuffer.splice(0);
      batch.forEach(({ level, msg }) => {
        invoke("write_log", { level, msg }).catch(() => {});
      });
      console.log("[logger] 已补发 " + batch.length + " 条启动早期日志");
    })
    .catch(() => {
      // IPC 仍未就绪, 500ms 后重试
      setTimeout(tryFlushBuffer, 500);
    });
}

// 页面加载后立即尝试刷新; 后续定时重试确保兜底
tryFlushBuffer();
setTimeout(tryFlushBuffer, 1000);
setTimeout(tryFlushBuffer, 3000);

// 挂钩 console 各方法: 原样输出到 WebView 控制台, 同时上报文件日志
["log", "info", "warn", "error", "debug"].forEach((key) => {
  const original = console[key] ? console[key].bind(console) : () => {};
  const level = key === "error" ? "E" : key === "warn" ? "W" : "I";
  console[key] = (...args) => {
    original(...args);
    sendLog(level, args);
  };
});

// 全局未捕获异常（黑屏排查关键: JS 启动崩溃会落在这里）
window.addEventListener("error", (e) => {
  sendLog("E", [
    `window.onerror: ${e.message} @ ${e.filename}:${e.lineno}:${e.colno}`,
  ]);
});

// 未处理的 Promise rejection（如 pdf.js 渲染失败）
window.addEventListener("unhandledrejection", (e) => {
  const reason = e.reason;
  sendLog("E", [
    `unhandledrejection: ${
      reason && reason.message ? reason.message : String(reason)
    }`,
  ]);
});

console.log("[logger] 前端日志上报已启用");
