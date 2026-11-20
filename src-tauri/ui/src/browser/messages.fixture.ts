/** Test-only copy for Task28; the parent task owns the application locales. */
export const browserTestMessages = {
  'en-US': {
    tauriPreview: {
      browser: {
        title: 'Isolated browser',
        intro: 'Browse manually in a private window restricted to the original HTTPS origin.',
        controls: 'Browser navigation',
        back: 'Back',
        forward: 'Forward',
        reload: 'Reload page',
        check: 'Check browser status',
        checking: 'Checking browser status…',
        closed: 'Browser window is closed.',
        opening: 'Opening browser window…',
        loading: 'Loading page…',
        opened: 'Browser window is open.',
        origin: 'Allowed origin',
        address: 'Current page',
        navigationBlocked: 'A navigation outside the allowed HTTPS origin was blocked.',
        downloads: 'Downloads are blocked in this isolated window. No files will be saved.',
        downloadBlocked: 'A download request was blocked. No file was saved.',
        historyUnavailable: 'Native back and forward controls are unavailable on this platform.',
        statusError: 'Unable to read browser status. Check again.',
        errors: {
          browser_invalid_url: 'Enter an HTTPS URL without embedded credentials.',
          browser_already_open: 'Close the current browser window before opening a new one.',
          browser_invalid_session: 'This browser window has changed. Check its status again.',
          browser_no_history: 'There is no page in that direction.',
          browser_history_unavailable: 'Native browser history is unavailable.',
          browser_unavailable: 'Unable to control the browser. Try again.'
        }
      }
    }
  },
  'zh-CN': {
    tauriPreview: {
      browser: {
        title: '隔离浏览器',
        intro: '在隐私窗口中手动浏览，仅允许最初的 HTTPS 同源页面。',
        controls: '浏览器导航',
        back: '后退',
        forward: '前进',
        reload: '刷新页面',
        check: '检查浏览器状态',
        checking: '正在检查浏览器状态…',
        closed: '浏览器窗口已关闭。',
        opening: '正在打开浏览器窗口…',
        loading: '正在加载页面…',
        opened: '浏览器窗口已打开。',
        origin: '允许的来源',
        address: '当前页面',
        navigationBlocked: '已阻止导航到允许的 HTTPS 来源之外。',
        downloads: '此隔离窗口禁止下载，不会保存任何文件。',
        downloadBlocked: '已阻止一次下载请求，未保存文件。',
        historyUnavailable: '当前平台不支持原生后退和前进控制。',
        statusError: '无法读取浏览器状态，请重新检查。',
        errors: {
          browser_invalid_url: '请输入不含嵌入凭据的 HTTPS 地址。',
          browser_already_open: '请先关闭当前浏览器窗口，再打开新窗口。',
          browser_invalid_session: '浏览器窗口已更换，请重新检查状态。',
          browser_no_history: '该方向没有可访问的历史页面。',
          browser_history_unavailable: '原生浏览器历史不可用。',
          browser_unavailable: '无法控制浏览器，请重试。'
        }
      }
    }
  }
} as const
