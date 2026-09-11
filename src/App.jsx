import { useState, useEffect, useRef } from "react";
import * as pdfjsLib from 'pdfjs-dist';
// Import worker entry - sets window.pdfjsWorker for fake worker mode (main thread)
// v3 is fully compatible with Chrome 110 / Android WebView
import 'pdfjs-dist/build/pdf.worker.entry';
import "./App.css";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

function App() {
  const imgRef = useRef(null);
  const slideBoxRef = useRef(null);
  const pdfDocRef = useRef(null);
  const [totalPage, setTotalPage] = useState(0);
  const [currentPage, setCurrentPage] = useState(1);
  const intervalRef = useRef(null);
  const [pdfData, setPdfData] = useState(null);

  // IoT 上传结果提示
  const [iotToast, setIotToast] = useState('');
  const iotToastTimer = useRef(null);

  // 设备信息弹窗状态
  const [showDeviceDialog, setShowDeviceDialog] = useState(false);
  const [deviceInfo, setDeviceInfo] = useState({ device_id: '', device_name: '', attachment_info: '' });
  const [deviceNameInput, setDeviceNameInput] = useState('');
  const [savingDeviceName, setSavingDeviceName] = useState(false);

  // 从 Rust 获取并加载 PDF
  const loadPdf = () => {
    console.log('[App] 请求 PDF 数据...');
    invoke('get_pdf_data').then(bytes => {
      console.log('[App] 收到 PDF 数据, 类型:', typeof bytes, '大小:', bytes?.length);
      
      if (bytes && bytes.length > 0) {
        try {
          const uint8 = new Uint8Array(bytes);
          console.log('[App] Uint8Array 大小:', uint8.length);
          const blob = new Blob([uint8], { type: 'application/pdf' });
          console.log('[App] Blob 大小:', blob.size);
          const blobUrl = URL.createObjectURL(blob);
          console.log('[App] Blob URL:', blobUrl);
          setPdfData(blobUrl);
        } catch (e) {
          console.error('[App] 处理 PDF 失败:', e);
        }
      } else {
        console.log('[App] 没有外部 PDF，加载内置 test.pdf');
      }
    }).catch(err => {
      console.error('[App] invoke get_pdf_data 失败:', err);
      console.log('[App] 回退到内置 test.pdf');
    });
  };

  useEffect(() => {
    loadPdf();
  }, []);

  useEffect(() => {
    if (!pdfData) return;
    console.log('Loading PDF from:', pdfData);

    const renderPage = async (num) => {
      if (!pdfDocRef.current) return;
      const page = await pdfDocRef.current.getPage(num);
      const scale = window.innerWidth / page.getViewport({ scale: 1 }).width;
      const viewport = page.getViewport({ scale });
      const canvas = document.createElement('canvas');
      const ctx = canvas.getContext('2d');
      canvas.height = viewport.height;
      canvas.width = viewport.width;
      await page.render({ canvasContext: ctx, viewport }).promise;
      if (imgRef.current) {
        imgRef.current.src = canvas.toDataURL('image/png');
      }
      setCurrentPage(num);
    };

    // 判断是 Blob URL 还是普通 URL
    const loadParam = pdfData.startsWith('blob:') ? { url: pdfData } : { url: pdfData };

    pdfjsLib.getDocument(loadParam).promise.then(doc => {
      pdfDocRef.current = doc;
      setTotalPage(doc.numPages);
      renderPage(1);
    }).catch(error => {
      console.error('Error loading PDF:', error);
      alert('无法加载PDF文件: ' + error.message);
    });

  }, [pdfData]);
  useEffect(() => {
    const unlisten = listen('page_change', (event) => {
      const pageNum = event.payload;
      console.log('Received page change event:', pageNum);
      
      // 停止自动翻页
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
        intervalRef.current = null;
      }
      
      // 渲染指定页
      if (pdfDocRef.current && pageNum >= 1 && pageNum <= totalPage) {
        const renderPage = async (num) => {
          const page = await pdfDocRef.current.getPage(num);
          const scale = window.innerWidth / page.getViewport({ scale: 1 }).width;
          const viewport = page.getViewport({ scale });
          const canvas = document.createElement('canvas');
          const ctx = canvas.getContext('2d');
          canvas.height = viewport.height;
          canvas.width = viewport.width;
          await page.render({ canvasContext: ctx, viewport }).promise;
          if (imgRef.current) {
            imgRef.current.src = canvas.toDataURL('image/png');
          }
          setCurrentPage(num);
        };
        renderPage(pageNum);
      }
    });
    
    return () => {
      unlisten.then(fn => fn());
    };
  }, [totalPage]);

  // 监听 WebSocket 连接状态
  useEffect(() => {
    const unlisten = listen('ws_status', (event) => {
      const msg = event.payload;
      console.log('[WS] 连接状态:', msg);
      setIotToast(msg);
      if (iotToastTimer.current) clearTimeout(iotToastTimer.current);
      iotToastTimer.current = setTimeout(() => setIotToast(''), 3000);
    });
    return () => {
      unlisten.then(fn => fn());
    };
  }, []);

  // 监听文件下载完成，重新加载 PDF
  useEffect(() => {
    const unlisten = listen('ws_data_synced', (event) => {
      const count = event.payload;
      console.log('[WS] 文件同步完成，共', count, '个文件，重新加载 PDF');
      setIotToast(`已同步 ${count} 个文件，加载中...`);
      if (iotToastTimer.current) clearTimeout(iotToastTimer.current);
      iotToastTimer.current = setTimeout(() => setIotToast(''), 3000);
      loadPdf();
    });
    return () => {
      unlisten.then(fn => fn());
    };
  }, []);
  
  // 打开设备信息弹窗
  const openDeviceDialog = async () => {
    try {
      const info = await invoke('get_device_info');
      setDeviceInfo(info);
      setDeviceNameInput(info.device_name || '');
      setShowDeviceDialog(true);
    } catch (err) {
      console.error('[Device] 获取设备信息失败:', err);
    }
  };

  // 关闭设备信息弹窗
  const closeDeviceDialog = () => {
    setShowDeviceDialog(false);
  };

  // 保存设备名称
  const handleSaveDeviceName = async () => {
    setSavingDeviceName(true);
    try {
      await invoke('save_device_name', { name: deviceNameInput });
      setDeviceInfo(prev => ({ ...prev, device_name: deviceNameInput }));
      setShowDeviceDialog(false);
    } catch (err) {
      console.error('[Device] 保存设备名称失败:', err);
      alert('保存失败: ' + err);
    } finally {
      setSavingDeviceName(false);
    }
  };
  
  return (
    <div id="slideBox" ref={slideBoxRef}>
      <img id="pageImg" ref={imgRef} />

      {/* 设备信息触发按钮 */}
      <button className="device-info-btn" onClick={openDeviceDialog} title="设备信息">
        <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
          <rect x="2" y="3" width="20" height="14" rx="2" ry="2"/>
          <line x1="8" y1="21" x2="16" y2="21"/>
          <line x1="12" y1="17" x2="12" y2="21"/>
        </svg>
      </button>

      {/* WebSocket 状态提示 */}
      {iotToast && (
        <div className={`iot-toast ${iotToast.includes('失败') || iotToast.includes('断开') ? 'iot-toast-error' : 'iot-toast-success'}`}>
          {iotToast}
        </div>
      )}

      {/* 设备信息弹窗 */}
      {showDeviceDialog && (
        <div className="dialog-overlay" onClick={closeDeviceDialog}>
          <div className="device-dialog" onClick={e => e.stopPropagation()}>
            <div className="dialog-header">
              <span className="dialog-title">设备信息</span>
              <button className="dialog-close" onClick={closeDeviceDialog}>×</button>
            </div>
            <div className="dialog-body">
              <div className="info-row">
                <span className="info-label">设备ID：</span>
                <span className="info-value">{deviceInfo.device_id || '未知'}</span>
              </div>
              <div className="info-row">
                <span className="info-label">设备名称：</span>
                <input
                  className="info-input"
                  type="text"
                  placeholder="请输入名称"
                  value={deviceNameInput}
                  onChange={e => setDeviceNameInput(e.target.value)}
                />
              </div>
              <div className="info-row">
                <span className="info-label">附件信息：</span>
                <span className="info-value">{deviceInfo.attachment_info || '无'}</span>
              </div>
            </div>
            <div className="dialog-footer">
              <button className="btn-cancel" onClick={closeDeviceDialog}>取消</button>
              <button className="btn-confirm" onClick={handleSaveDeviceName} disabled={savingDeviceName}>
                {savingDeviceName ? '保存中...' : '确定'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

export default App;
