//fake electronAPI
  // ---------- Tauri 2.x 全局 API 安全获取 ----------
  if (typeof window.__TAURI__ === 'undefined') {
    // 纯浏览器调试时，给个空对象防报错
    window.__TAURI__ = { core: { invoke: () => {} }, event: { listen: () => {} } };
  }
  // 2.x 中 invoke 在 core 命名空间下
  const invoke = window.__TAURI__.core.invoke;
  const { listen } = window.__TAURI__.event;

  // ---------- 完整模拟原 preload.js 的 electronAPI ----------
  window.electronAPI = {
    isElectron: true,

    // 窗口/会话操作
    openAudioChat: (payload) => invoke('audio-chat:open', payload),
    openDesktopWork: (payload) => invoke('desktop-work:open', payload),
    getAudioChatSession: () => invoke('audio-chat:get-session'),
    getDesktopWorkSession: () => invoke('desktop-work:get-session'),
    checkpointAudioChat: (turns) => invoke('audio-chat:checkpoint', turns),
    completeAudioChat: (turns) => invoke('audio-chat:complete', turns),
    completeDesktopWork: (turns) => invoke('desktop-work:complete', turns),

    // 事件监听（后端推送）
    onAudioChatCompleted: (callback) => {
      const unlisten = listen('ipc-back:audio-chat:completed', (e) => callback(e.payload));
      return () => unlisten.then(f => f());
    },
    onTrayAction: (callback) => {
      const unlisten = listen('ipc-back:tray:action', (e) => callback(e.payload));
      return () => unlisten.then(f => f());
    },
    onDesktopScreenshotCaptured: (callback) => {
      const unlisten = listen('ipc-back:desktop-work:screenshot-captured', (e) => callback(e.payload));
      return () => unlisten.then(f => f());
    },
    onDesktopScreenshotError: (callback) => {
      const unlisten = listen('ipc-back:desktop-work:screenshot-error', (e) => callback(e.payload));
      return () => unlisten.then(f => f());
    },

    // 讯飞语音
    startXunfei: (credentials) => invoke('xunfei:start', credentials),
    sendXunfeiAudio: (samples) => {
      invoke('xunfei:audio', { samples: Array.from(samples || []) }).catch(e => console.warn(e));
    },
    finishXunfei: () => invoke('xunfei:finish'),
    abortXunfei: () => invoke('xunfei:abort'),
    onXunfeiPartial: (callback) => {
      const unlisten = listen('ipc-back:xunfei:partial', (e) => callback(e.payload));
      return () => unlisten.then(f => f());
    },
    onXunfeiFinal: (callback) => {
      const unlisten = listen('ipc-back:xunfei:final', (e) => callback(e.payload));
      return () => unlisten.then(f => f());
    },
    onXunfeiError: (callback) => {
      const unlisten = listen('ipc-back:xunfei:error', (e) => callback(e.payload));
      return () => unlisten.then(f => f());
    },

    // TTS 与预设
    generateVoicePreset: (config) => invoke('audio-chat:generate-preset', config),
    getVoicePreset: (config) => invoke('audio-chat:get-preset', config),
    getDesktopVoicePreset: (config) => invoke('desktop-work:get-preset', config),
    requestFishSpeech: (request) => invoke('audio-chat:tts', request),
    requestDesktopFishSpeech: (request) => invoke('desktop-work:tts', request),

    // 桌面交互
    setDesktopWorkInteractive: (interactive) => invoke('desktop-work:set-interactive', interactive),

    // 生命周期通知
    signalRendererReady: () => invoke('renderer:ready'),
    updateAppState: (state) => invoke('app-state:update', state),
  };

  window.ipcRenderer = window.electronAPI;
  console.log('✅ [Shim] Tauri 2.x electronAPI mocked');