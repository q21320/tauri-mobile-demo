// Polyfills MUST be first import for WebView compatibility
import "./polyfills.js";
// 日志上报第二: 后续所有 console 与全局异常转发到设备日志文件
import "./logger.js";

import ReactDOM from "react-dom/client";
import App from "./App";

console.log("[boot] main.jsx 开始执行, UA:", navigator.userAgent);

ReactDOM.createRoot(document.getElementById("root")).render(
  <App />
);
console.log("[boot] React render 已调用");
