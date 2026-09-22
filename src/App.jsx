import { useState, useEffect, useRef, useCallback } from "react";
import * as pdfjsLib from 'pdfjs-dist';
// pdfjs-dist v2.16.105 - 兼容 Chrome 83
import 'pdfjs-dist/build/pdf.worker.entry';
import "./App.css";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

function App() {
  const slideBoxRef = useRef(null);
  const pdfDocRef = useRef(null);        // { doc, fileName } 缓存 pdfjs 文档和文件名
  const [totalPage, setTotalPage] = useState(0);
  const [currentPage, setCurrentPage] = useState(1);
  const pdfDataRef = useRef(null);       // 避免不必要的 state 变更
  const pendingPageRef = useRef(null);   // pdf_show 待渲染的页码

  // 预渲染相关
  const [pageImages, setPageImages] = useState([]);   // 所有页面的 data URL 数组
  const pageImagesRef = useRef([]); // 用 ref 追踪 pageImages，避免闭包陈旧值
  const renderedPagesRef = useRef([]); // 当前已渲染的页码列表（1-based）
  const cancelRenderRef = useRef(false); // 取消当前渲染标记
  const [renderProgress, setRenderProgress] = useState(''); // 预渲染进度文本
  const isRenderingRef = useRef(false); // 是否正在预渲染中
  const initializedRef = useRef(false); // StrictMode 防重复初始化
  const loadingFileRef = useRef(null);  // 正在加载中的文件名，防止重复 loadPdf

  // 去重：上一次处理的 fileName + page 组合
  const lastShowRef = useRef(null);      // { fileName, page }
  const debounceTimerRef = useRef(null); // pdf_show 防抖定时器

  // IoT 上传结果提示
  const [iotToast, setIotToast] = useState('');
  const iotToastTimer = useRef(null);

  const handleTap = useCallback((e) => {
    if (e.target.closest('.device-info-btn')) return;
    const x = e.clientX;
    if (x === undefined) return;
    const nextPage = x < window.innerWidth / 2 ? currentPage - 1 : currentPage + 1;
    if (nextPage >= 1 && nextPage <= totalPage) {
      setCurrentPage(nextPage);
    }
  }, [currentPage, totalPage]);

  // 设备信息弹窗状态
  const [showDeviceDialog, setShowDeviceDialog] = useState(false);
  const [deviceInfo, setDeviceInfo] = useState({ device_id: '', device_name: '', attachment_info: '' });
  const [deviceNameInput, setDeviceNameInput] = useState('');
  const [savingDeviceName, setSavingDeviceName] = useState(false);

  // 预渲染页面：targetPages 为指定页码数组（1-based），不传则渲染全部
  const renderAllPages = useCallback(async (pdfDoc, initialPage, targetPages) => {
    // 取消正在进行的渲染
    if (isRenderingRef.current) {
      console.log('[App] renderAllPages: 取消正在进行的渲染任务');
      cancelRenderRef.current = true;
    }
    isRenderingRef.current = true;
    cancelRenderRef.current = false;

    const totalPages = pdfDoc.numPages;
    const pagesToRender = targetPages || Array.from({ length: totalPages }, (_, i) => i + 1);
    const pageImagesArr = [];
    const renderedPageNums = [];
    const t0 = performance.now();

    console.log('[App] 开始渲染, 目标页数:', pagesToRender.length, '/', totalPages, ', 初始页:', initialPage);

    try {
      for (let idx = 0; idx < pagesToRender.length; idx++) {
        if (cancelRenderRef.current) {
          console.log('[App] 渲染任务已取消');
          isRenderingRef.current = false;
          return;
        }

        const pageNum = pagesToRender[idx];
        setRenderProgress(`正在渲染第 ${idx + 1}/${pagesToRender.length} 页...`);

        const page = await pdfDoc.getPage(pageNum);
        const uw = page.getViewport({ scale: 1 }).width;
        const uh = page.getViewport({ scale: 1 }).height;
        const sx = window.innerWidth / uw;
        const sy = window.innerHeight / uh;
        let s = Math.min(sx, sy, 1.5);
        const vp = page.getViewport({ scale: s });

        const canvas = document.createElement('canvas');
        canvas.width = vp.width;
        canvas.height = vp.height;
        const ctx = canvas.getContext('2d');
        await page.render({ canvasContext: ctx, viewport: vp }).promise;

        pageImagesArr[idx] = canvas.toDataURL('image/png');
        renderedPageNums[idx] = pageNum;
        console.log(`[App] 预渲染: 第 ${idx + 1}/${pagesToRender.length} 页完成 (实际第 ${pageNum} 页)`);
      }

      const elapsed = (performance.now() - t0).toFixed(0);
      console.log(`[App] ${pagesToRender.length} 页预渲染完成, 耗时 ${elapsed} ms`);

      // 全部完成后才显示
      pageImagesRef.current = [...pageImagesArr];
      renderedPagesRef.current = [...renderedPageNums];
      setPageImages([...pageImagesArr]);
      // currentPage 存储的是 pageImages 数组的 1-based 索引
      const displayPageNum = pagesToRender.includes(initialPage) ? initialPage : pagesToRender[0];
      const displayIdx = renderedPageNums.indexOf(displayPageNum);
      setCurrentPage(displayIdx !== -1 ? displayIdx + 1 : 1);
      setRenderProgress('');
      setTotalPage(totalPages);

      // 处理 pending page
      if (pendingPageRef.current) {
        const p = pendingPageRef.current;
        pendingPageRef.current = null;
        const pIdx = renderedPageNums.indexOf(p);
        if (pIdx !== -1) {
          setCurrentPage(pIdx + 1);
        }
      }
    } catch (e) {
      console.error('[App] renderAllPages 出错:', e);
      setRenderProgress('');
    } finally {
      isRenderingRef.current = false;
    }
  }, []);

  // 加载 PDF 数据并解析（仅在文件切换时调用）
  // fileName 不再由前端传入，而是从 Rust 端返回，消除 __initial__ hack
  const loadPdf = useCallback((targetPage) => {
    // 防止并发加载（有任何加载进行中则只记录目标页码）
    if (loadingFileRef.current) {
      console.log('[App] loadPdf: 文件正在加载中, 跳过');
      pendingPageRef.current = targetPage || pendingPageRef.current;
      return;
    }
    loadingFileRef.current = true;
    console.log('[App] 请求 PDF 数据...');
    setRenderProgress('正在获取 PDF 数据...');
    invoke('get_pdf_data').then(result => {
      console.log('[App] 收到 PDF 数据, fileName:', result?.file_name, 'base64长度:', result?.base64?.length);

      if (result) {
        const { file_name, base64 } = result;
        // 如果该文件已经加载完成，跳过重复加载
        if (pdfDocRef.current?.fileName === file_name && pdfDocRef.current?.doc) {
          console.log('[App] loadPdf: 文件已加载, 跳过重复加载:', file_name);
          loadingFileRef.current = null;
          const page = targetPage || 1;
          if (pageImagesRef.current.length > 0) {
            const idx = renderedPagesRef.current.indexOf(page);
            setCurrentPage(idx !== -1 ? idx + 1 : 1);
          } else {
            pendingPageRef.current = page;
          }
          return;
        }
        try {
          // base64 → 二进制
          const binary = atob(base64);
          const uint8 = new Uint8Array(binary.length);
          for (let i = 0; i < binary.length; i++) {
            uint8[i] = binary.charCodeAt(i);
          }
          console.log('[App] Uint8Array 大小:', uint8.length);
          pdfDataRef.current = uint8;

          // 解析 PDF 文档
          const loadParam = { data: uint8 };
          console.log('[App] pdfjs 加载方式: data (Uint8Array), 大小:', uint8.length);
          const t0 = performance.now();

          pdfjsLib.getDocument(loadParam).promise.then(doc => {
            const elapsed = (performance.now() - t0).toFixed(0);
            console.log('[App] pdfjs 解析完成, 耗时', elapsed, 'ms, 总页数:', doc.numPages);
            loadingFileRef.current = null; // 加载完成，清除加载中标记
            pdfDocRef.current = { doc, fileName: file_name }; // 直接使用 Rust 返回的文件名
            setTotalPage(doc.numPages);

            // 预渲染所有页面
            const page = targetPage || 1;
            console.log('[App] 加载完成后预渲染所有页, 目标第', page, '页');
            renderAllPages(doc, page);
          }).catch(error => {
            console.error('Error loading PDF:', error);
            loadingFileRef.current = null;
            setRenderProgress('');
            alert('无法加载PDF文件: ' + error.message);
          });
        } catch (e) {
          console.error('[App] base64 解码失败:', e);
          loadingFileRef.current = null;
          setRenderProgress('');
        }
      } else {
        console.log('[App] 没有外部 PDF 数据');
        loadingFileRef.current = null;
        setRenderProgress('');
      }
    }).catch(err => {
      console.error('[App] invoke get_pdf_data 失败:', err);
      loadingFileRef.current = null;
      setRenderProgress('');
    });
  }, [renderAllPages]);

  // 处理 pdf_show 事件的核心逻辑（含去重）
  const handlePdfShow = useCallback((fileName, page, targetPages) => {
    // 去重：相同的 fileName + page 组合直接忽略
    const last = lastShowRef.current;
    if (last && last.fileName === fileName && last.page === page) {
      console.log('[App] pdf_show 去重: 忽略重复的', fileName, '第', page, '页');
      return;
    }
    lastShowRef.current = { fileName, page };

    console.log('[App] 处理 pdf_show:', fileName, '第', page, '页, targetPages:', targetPages);

    // 如果文档已加载且是同一文件
    const isSameFile = pdfDocRef.current?.doc && pageImagesRef.current.length > 0 && pdfDocRef.current.fileName === fileName;
    if (isSameFile) {
      // 检查目标页是否已在已渲染列表中
      const renderedIdx = renderedPagesRef.current.indexOf(page);
      if (renderedIdx !== -1) {
        console.log('[App] 目标页已渲染, 直接切换显示第', page, '页 (索引', renderedIdx, ')');
        setCurrentPage(renderedIdx + 1); // 1-based index
        return;
      }
      // 目标页未渲染，需要重新渲染指定页面
      console.log('[App] 目标页未渲染, 重新渲染 targetPages:', targetPages);
      pageImagesRef.current = [];
      renderedPagesRef.current = [];
      setPageImages([]);
      renderAllPages(pdfDocRef.current.doc, page, targetPages);
    } else if (isRenderingRef.current || loadingFileRef.current) {
      // 有加载或预渲染在进行中，只记录目标页码
      console.log('[App] 加载/预渲染进行中, 目标页设为第', page, '页');
      pendingPageRef.current = page;
    } else {
      // 真正需要切换文件（没有加载在进行，且文件不同）
      console.log('[App] 切换文件, 重新加载:', fileName);
      pdfDocRef.current = { doc: null, fileName };
      pageImagesRef.current = [];
      renderedPagesRef.current = [];
      setPageImages([]); // 清除旧页面数据，显示加载遮罩
      pendingPageRef.current = page;
      loadPdf(page);
    }
  }, [loadPdf, renderAllPages]);

  // 启动时加载 PDF（作为后备，确保首次加载不依赖 pdf_show 事件时序）
  // 使用 initializedRef 防止 StrictMode 双重挂载导致重复加载
  useEffect(() => {
    if (initializedRef.current) {
      console.log('[App] StrictMode 重复挂载, 跳过初始化');
      return;
    }
    initializedRef.current = true;
    console.log('[App] 已挂载, 视口:', window.innerWidth, 'x', window.innerHeight);
    console.log('[App] UA:', navigator.userAgent);
    console.log('[App] 初始加载 PDF...');
    loadPdf(1);
  }, [loadPdf]);

  // 监听 page_change 事件（翻页，不切换文件）
  useEffect(() => {
    const unlisten = listen('page_change', (event) => {
      const pageNum = event.payload;
      console.log('[App] Received page_change event:', pageNum);

      // 预渲染已完成，直接切换页码
      if (pdfDocRef.current?.doc && pageNum >= 1 && pageNum <= totalPage && pageImagesRef.current.length > 0) {
        const idx = renderedPagesRef.current.indexOf(pageNum);
        if (idx !== -1) {
          setCurrentPage(idx + 1); // 1-based index
        }
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

  // 监听 ws_data_synced 事件，文件下载完成后重新加载 PDF
  useEffect(() => {
    const offWsDataSynced = listen('ws_data_synced', (event) => {
      console.log('[App] 收到 ws_data_synced 事件, 重新加载 PDF');
      // 清除当前缓存，强制重新加载
      pdfDocRef.current = null;
      pageImagesRef.current = [];
      renderedPagesRef.current = [];
      setPageImages([]);
      loadPdf(1);
    });
    return () => {
      offWsDataSynced.then(fn => fn());
    };
  }, [loadPdf]);

  // 监听 pdf_show 事件：带防抖（300ms）
  useEffect(() => {
    const unlisten = listen('pdf_show', (event) => {
      const { fileName, page, targetPages } = event.payload;
      console.log('[App] 收到 pdf_show:', fileName, '第', page, '页, targetPages:', targetPages);

      // 防抖：清除之前的定时器，300ms 后执行
      if (debounceTimerRef.current) {
        clearTimeout(debounceTimerRef.current);
      }
      debounceTimerRef.current = setTimeout(() => {
        debounceTimerRef.current = null;
        handlePdfShow(fileName, page, targetPages);
      }, 300);
    });
    return () => {
      unlisten.then(fn => fn());
      if (debounceTimerRef.current) {
        clearTimeout(debounceTimerRef.current);
      }
    };
  }, [handlePdfShow]);

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
    <div id="slideBox" ref={slideBoxRef} onClick={handleTap}>
      {/* 加载提示 - 当前页图片未就绪时显示 */}
      {(pageImages.length === 0 || (currentPage >= 1 && currentPage <= pageImages.length && !pageImages[currentPage - 1])) && (
        <div className="loading-overlay">
          <div className="loading-text">{renderProgress || '正在加载 PDF...'}</div>
        </div>
      )}

      {/* 页码提示 */}
      {totalPage > 0 && pageImages.length > 0 && (
        <div className="page-hint">{currentPage} / {totalPage}</div>
      )}

      {/* 显示当前页 - 预渲染完成后用 img 展示（需确保当前页图片已渲染） */}
      {pageImages.length > 0 && currentPage >= 1 && currentPage <= pageImages.length && pageImages[currentPage - 1] && (
        <img
          id="pageCanvas"
          src={pageImages[currentPage - 1]}
          alt={`Page ${renderedPagesRef.current[currentPage - 1] || currentPage}`}
        />
      )}

      {/* 设备信息触发按钮 */}
      <button className="device-info-btn" onClick={openDeviceDialog} title="设备信息">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
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
