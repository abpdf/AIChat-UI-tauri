// fake-electron-api.js
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

// 全局挂载 electronAPI（兼容旧代码）
window.electronAPI = {
  isElectron: true,

  // ====== 窗口与协作 ======
  openAudioChat: (payload) => invoke('audio_chat_open', { payload }),
  openDesktopWork: (payload) => invoke('desktop_work_open', { payload }),

  // ====== 会话状态 ======
  getAudioChatSession: () => invoke('audio_chat_get_session'),
  getDesktopWorkSession: () => invoke('desktop_work_get_session'),
  checkpointAudioChat: (turns) => invoke('audio_chat_checkpoint', { turns }),
  completeAudioChat: (turns) => invoke('audio_chat_complete', { turns }),
  completeDesktopWork: (turns) => invoke('desktop_work_complete', { turns }),

  // ====== 事件监听 ======
  onAudioChatCompleted: (callback) => {
    const unlistenPromise = listen('audio-chat:completed', (event) => callback(event.payload));
    return () => unlistenPromise.then((unlisten) => unlisten());
  },
  onDesktopScreenshotCaptured: (callback) => {
    const unlistenPromise = listen('desktop-work:screenshot-captured', (event) => callback(event.payload));
    return () => unlistenPromise.then((unlisten) => unlisten());
  },
  onDesktopScreenshotError: (callback) => {
    const unlistenPromise = listen('desktop-work:screenshot-error', (event) => callback(event.payload));
    return () => unlistenPromise.then((unlisten) => unlisten());
  },
  onTrayAction: (callback) => {
    const unlistenPromise = listen('tray:action', (event) => callback(event.payload));
    return () => unlistenPromise.then((unlisten) => unlisten());
  },

  // ====== Fish Audio 语音合成 ======
  generateVoicePreset: (config) => invoke('audio_chat_generate_preset', { config }),
  getVoicePreset: (config) => invoke('audio_chat_get_preset', { config }),
  getDesktopVoicePreset: (config) => invoke('desktop_work_get_preset', { config }),
  requestFishSpeech: (request) => invoke('audio_chat_tts', { request }),
  requestDesktopFishSpeech: (request) => invoke('desktop_work_tts', { request }),

  // ====== 桌面协作控制 ======
  setDesktopWorkInteractive: (interactive) => invoke('desktop_work_set_interactive', { value: interactive }),

  // ====== 主窗口状态同步 ======
  signalRendererReady: () => invoke('renderer_ready'),
  updateAppState: (state) => invoke('app_state_update', { stateValue: state }),

  // ====== 讯飞语音识别 ======
  startXunfei: (credentials) => invoke('xunfei_start', { credentials }),
  sendXunfeiAudio: (samples) => invoke('xunfei_audio', { samples: Array.from(samples || []) }),
  finishXunfei: () => invoke('xunfei_finish'),
  abortXunfei: () => invoke('xunfei_abort'),

  onXunfeiPartial: (callback) => {
    const unlistenPromise = listen('xunfei:partial', (event) => callback(event.payload));
    return () => unlistenPromise.then((unlisten) => unlisten());
  },
  onXunfeiFinal: (callback) => {
    const unlistenPromise = listen('xunfei:final', (event) => callback(event.payload));
    return () => unlistenPromise.then((unlisten) => unlisten());
  },
  onXunfeiError: (callback) => {
    const unlistenPromise = listen('xunfei:error', (event) => callback(event.payload));
    return () => unlistenPromise.then((unlisten) => unlisten());
  },
};