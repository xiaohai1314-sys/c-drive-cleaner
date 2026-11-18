// WebView2 宿主对 window.__TAURI__ 的兼容层。
// 必须在页面脚本之前注入（AddScriptToExecuteOnDocumentCreated）。
(function () {
  if (window.__TAURI__) {
    return;
  }

  var pending = new Map();
  var sequence = 1;

  function send(message) {
    window.chrome.webview.postMessage(message);
  }

  window.chrome.webview.addEventListener("message", function (event) {
    var data = event.data;
    if (typeof data === "string") {
      try {
        data = JSON.parse(data);
      } catch (error) {
        return;
      }
    }
    if (!data || typeof data.id === "undefined") {
      return;
    }
    var entry = pending.get(data.id);
    if (!entry) {
      return;
    }
    pending.delete(data.id);
    if (data.ok) {
      entry.resolve(data.data);
    } else {
      entry.reject(data.error);
    }
  });

  function invoke(cmd, args) {
    return new Promise(function (resolve, reject) {
      var id = sequence++;
      pending.set(id, { resolve: resolve, reject: reject });
      send({ id: id, cmd: cmd, args: args == null ? {} : args });
    });
  }

  function convertFileSrc(path, protocol) {
    if (protocol === "imthumb") {
      return (
        "https://imthumb.cdrivecleaner.invalid/?p=" + encodeURIComponent(path)
      );
    }
    return path;
  }

  var currentWindow = {
    minimize: function () {
      send({ cmd: "__win_minimize" });
    },
    toggleMaximize: function () {
      send({ cmd: "__win_toggle_maximize" });
    },
    close: function () {
      send({ cmd: "__win_close" });
    },
  };

  window.__TAURI__ = {
    core: {
      invoke: invoke,
      convertFileSrc: convertFileSrc,
    },
    window: {
      getCurrentWindow: function () {
        return currentWindow;
      },
    },
    dialog: {
      open: function (options) {
        return invoke("__dialog_open", options);
      },
    },
  };

  // 与 Tauri 注入脚本一致：只处理带 data-tauri-drag-region 的目标本身，
  // 窗口按钮和 .brand 内的图标/文字不触发拖拽。
  document.addEventListener(
    "mousedown",
    function (event) {
      if (event.button !== 0) {
        return;
      }
      var target = event.target;
      if (
        target &&
        target.hasAttribute &&
        target.hasAttribute("data-tauri-drag-region")
      ) {
        event.preventDefault();
        send({ cmd: "__win_drag" });
      }
    },
    true,
  );

  document.addEventListener(
    "dblclick",
    function (event) {
      var target = event.target;
      if (
        target &&
        target.hasAttribute &&
        target.hasAttribute("data-tauri-drag-region")
      ) {
        send({ cmd: "__win_toggle_maximize" });
      }
    },
    true,
  );
})();
