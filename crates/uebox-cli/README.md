# UEBox native CLI

Extract the entire archive and add its directory to your account's PATH, or
run the executable by its absolute path. In the running UEBox Tauri application,
open **Native CLI** and start the local endpoint. On macOS and Linux the
console is named `uebox`. On Windows it is `uebox.exe`.

```sh
uebox --version
uebox doctor --json
uebox projects list --json
uebox tools list --json
```

If you run one or more UEBox instances, pass the exact endpoint address
displayed by the selected window using `--endpoint`. Help and version work
offline. The other commands use the running Tauri application and an explicitly
connected editor.

To enable `ask`, bind a model to the assistant in **Model settings** and
enable CLI questions before starting the endpoint. Pass its model ID with
`--model`. The configured provider receives the question and public results
from the selected editor's enabled read-only tools.

# UEBox 原生命令行

解压后将该目录添加到用户的 PATH，也可直接运行命令的绝对路径。在运行的 UEBox Tauri
应用中打开「原生 CLI」并启动入口。macOS 和 Linux 的程序名为 `uebox`，Windows 为
`uebox.exe`。

可通过 `--endpoint` 指定主窗口显示的地址。帮助和版本命令可离线运行。其他命令通过
运行中的应用读取已经连接的编辑器。

使用 `ask` 前，在「模型设置」为助手绑定模型并启用 CLI 问答，使用 `--model` 传入
该模型的登记 ID。问题和所选编辑器工具的公开只读结果会发给配置的模型服务。
