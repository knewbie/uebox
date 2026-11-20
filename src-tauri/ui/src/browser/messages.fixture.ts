/** Test-only copy for Task28; the parent task owns the application locales. */
export const browserTestMessages = {
  'en-US': {
    tauriPreview: {
      browser: {
        downloadUrl: 'Same-origin download URL',
        saveLink: 'Save link as file',
        downloading: 'Saving file…',
        downloadSaved: 'File saved.',
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
        downloads: 'Use the download field to save a same-origin file.',
        downloadBlocked: 'A download request was blocked. No file was saved.',
        historyUnavailable: 'Native back and forward controls are unavailable on this platform.',
        statusError: 'Unable to read browser status. Check again.',
        errors: {
          browser_download_failed: 'Unable to download this link.',
          browser_download_busy: 'A download is already in progress.',
          browser_download_size: 'Choose a file no larger than 128 MiB.',
          browser_download_destination: 'Choose a writable destination and a new filename.',
          browser_download_origin: 'Use an HTTPS link from this browser’s allowed origin.',
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
        downloadUrl: '同源下载地址',
        saveLink: '将链接保存为文件',
        downloading: '正在保存文件…',
        downloadSaved: '文件已保存。',
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
        downloads: '可通过同源下载地址保存文件。',
        downloadBlocked: '已阻止一次下载请求，未保存文件。',
        historyUnavailable: '当前平台不支持原生后退和前进控制。',
        statusError: '无法读取浏览器状态，请重新检查。',
        errors: {
          browser_download_failed: '无法下载此链接。',
          browser_download_busy: '已有下载正在运行。',
          browser_download_size: '请选择不超过 128 MiB 的文件。',
          browser_download_destination: '请选择可写入的目录和新文件名。',
          browser_download_origin: '请输入此隔离浏览器允许的同源 HTTPS 链接。',
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
