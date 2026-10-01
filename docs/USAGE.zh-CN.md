# 用法

命令、参数，和故障排除。安装和构建见 [README](../README.zh-CN.md)。

## 语法

```
landrop-cli [OPTIONS] <COMMAND>
```

### 全局选项

| 选项 | 说明 |
|---|---|
| `--json` | 在 stdout 输出机器可读的 JSON。隐含非交互：不会有任何提示 |
| `-q`, `--quiet` | 后台/服务模式。无横幅、无进度条；警告和错误仍输出到 stderr |
| `--config-dir <DIR>` | 使用其它配置目录。便于测试，也便于同时运行多个身份 |
| `-h`, `--help` | 查看帮助 |
| `-V`, `--version` | 查看版本 |

所有命令都接受这些选项，放在子命令前后都可以。

## 命令一览

| 命令 | 作用 |
|---|---|
| [`discover`](#discover) | 发现局域网内的设备 |
| [`send`](#send) | 发送文件或目录 |
| [`receive`](#receive) | 接收：作为服务端等待他人发送 |
| [`text`](#text) | 发送一段短文本 |
| [`devices`](#devices) | 列出、查找或遗忘此前见过的设备 |
| [`trusted`](#trusted) | 管理 `receive` 免确认接受的发送方 |
| [`identity`](#identity) | 查看本 CLI 的身份、配置与路径 |
| [`selftest`](#selftest) | 不涉及网络对端，端到端自检 |

---

### `discover`

发现局域网设备，并记住它们以供后续按名字查找。

| 选项 | 默认 | 说明 |
|---|---|---|
| `--timeout <SECS>` | `3` | 监听回复的时长 |
| `--loopback` | 关 | 纳入环回接口(同机测试用) |
| `--discovery-port <PORT>` | `52637` | 仅在隔离测试时改动 |

```console
$ landrop-cli discover
$ landrop-cli discover --timeout 5 --json
```

每次运行只报告**本次回应了**的设备。记住的设备单独保存，由 [`devices`](#devices) 列出。

---

### `send`

```console
landrop-cli send [OPTIONS] [FILES]...
```

| 选项 | 默认 | 说明 |
|---|---|---|
| `-t`, `--to <TARGET>` | — | 发送目标。**逗号分隔可发给多台**。不带则打开交互式选择菜单 |
| `--stdin` | 关 | 从标准输入按行读取路径，而不是用参数 |
| `--first` | 关 | 取发现到的第一台设备，不提示 |
| `--reply-timeout <SECS>` | `300` | 等待对端接受的最长秒数 |
| `-y`, `--yes` | 关 | 跳过本地的"确认发送？" |
| `--wait-for-close` | 关 | 发完后等待对端关闭连接，见下 |

`--to` 支持三种形式：

| 形式 | 示例 | 说明 |
|---|---|---|
| 公钥 | `--to AkxBTkRS...` | 最可靠。44 个 base64 字符，跨运行稳定 |
| 设备名 | `--to build-server` | 需要此前 `discover` 过，或对方正在回应 |
| `host:port` | `--to 192.168.1.42:44769` | 完全跳过发现 |

目录会递归发送，接收端会重建相同的目录结构。空文件按零字节文件传输。

**多目标。** `--to a,b,c` 可一次发给多台。**所有目标会先全部解析再发送**,所以名字打错
会让整次运行停止，而不是让前几台收到一半。传输仍是一台接一台进行，各自等待自己的接受
确认 —— 因此某一台失败不会拖累其余。同一台设备写两次只会发一次。

```console
$ landrop-cli send report.pdf --to build-server
$ landrop-cli send ./photos --to AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u
$ landrop-cli send a.zip b.zip --to laptop,phone --json
$ find . -name '*.log' | landrop-cli send --stdin --to build-server
```

**关于 `--wait-for-close`。** 一次发送正常在**字节交给操作系统**时就返回，这可能远早于
对端把它们写入磁盘 —— 发送端的进度条先跑完就是这个原因。加上这个选项后，它会改为等待
对端关闭连接，这是最接近"文件已落到对方磁盘"的信号。默认关闭，因为没有任何约定要求
接收端必须关闭连接，所以它**只能尽力而为**,并且会让命令更慢。结果里会多出
`peer_closed`:`true` 表示对端已关闭，`false` 表示等待超时，`null` 表示没有加这个选项。

---

### `receive`

前台运行，直到被停止。连接是**并发处理**的，所以某台设备卡在接受提示上不会挡住下一台。

| 选项 | 默认 | 说明 |
|---|---|---|
| `--port <PORT>` | 上次记住的 | TCP 端口。传入的值会被记住；**`--port 0`** 表示每次随机取端口并忘掉记住的那个 |
| `--dir <DIR>` | 已配置的 | 接收文件的保存目录 |
| `--name <NAME>` | 主机名 | 对外通告的设备名 |
| `-y`, `--yes` | 关 | 不经确认接受所有传输 |
| `--discovery-port <PORT>` | `52637` | 仅在隔离测试时改动 |
| `--once` | 关 | 处理完一次传输后退出 |
| `--max <N>` | `0`(不限) | 处理完 N 次传输后退出 |

```console
$ landrop-cli receive
$ landrop-cli receive --dir ~/inbox --yes
$ landrop-cli receive --port 41234 --max 3
```

不加 `--yes` 时，来自**信任列表之外**的发送方会弹确认。若以服务方式启动(没有终端),
这些请求会被**直接拒绝**,所以请先把预期的发送方加入信任列表。

---

### `text`

```console
landrop-cli text [OPTIONS] <TEXT>
```

| 选项 | 说明 |
|---|---|
| `-t`, `--to <TARGET>` | 形式同 `send`。不带则打开交互式选择菜单 |
| `--first` | 取发现到的第一台设备，不提示 |

```console
$ landrop-cli text 'build finished' --to build-server
```

---

### `devices`

此前记住的设备，供 `--to` 按名字查找。

| 选项 | 说明 |
|---|---|
| `--forget <NAME\|KEY>` | 按名字或公钥移除一台已记住的设备 |

```console
$ landrop-cli devices
$ landrop-cli devices --forget old-laptop
```

---

### `trusted`

`receive` 免确认接受的发送方。

| 选项 | 说明 |
|---|---|
| `--add <NAME\|KEY>` | 信任一个发送方。用名字时，该名字必须能解析到见过的设备 |
| `--remove <NAME\|KEY>` | 不再信任某个发送方 |
| `--edit <NAME\|KEY> --name <NEW>` | 给已信任的发送方改名。密钥不变，信任关系也不变。若 `NEW` 已是别的条目在用，会被拒绝 |

```console
$ landrop-cli trusted
$ landrop-cli trusted --add build-server
$ landrop-cli trusted --edit build-server --name workshop-pc
$ landrop-cli trusted --remove workshop-pc
```

---

### `identity`

| 选项 | 说明 |
|---|---|
| `--show-secret` | 同时打印私钥。请妥善保管 |

```console
$ landrop-cli identity
```

---

### `selftest`

在环回上传输一段数据，不涉及任何对端。**这一步失败说明问题在本地。**

| 选项 | 默认 | 说明 |
|---|---|---|
| `--size <BYTES>` | `5242880` | 数据大小 |

```console
$ landrop-cli selftest
```

---

## 退出码

| 码 | 含义 |
|---|---|
| `0` | 成功。配合 `--json` 时 `status` 为 `ok` |
| `1` | 失败。配合 `--json` 时 `status` 为 `error`,`error` 说明原因 |
| `2` | 命令行有误 —— 未知选项或缺参数 |

使用多个 `--to` 目标时，**只要有一台失败退出码就非零**,所以要读逐台的结果条目，
而不是只看退出码。

## 文件放在哪里

| 平台 | 目录 |
|---|---|
| Linux | `~/.config/landrop-cli` |
| Windows | `%APPDATA%\landrop-cli` |
| macOS | `~/Library/Application Support/landrop-cli` |

`landrop-cli identity` 会打印实际使用的路径。用 `--config-dir` 可以改写。该目录包含：

| 文件 | 内容 |
|---|---|
| `identity.json` | 本 CLI 的身份密钥。**请妥善保管** —— 其它设备就是靠它识别你的 |
| `settings.json` | 设备名、下载目录、监听端口、信任列表 |
| `devices.json` | 此前记住的设备 |

## 后台运行

`--quiet` 会去掉横幅和进度条，但保留警告和错误，因此它是服务管理器下的正确模式。

```ini
# ~/.config/systemd/user/landrop-cli.service
[Unit]
Description=LANDrop CLI receiver

[Service]
ExecStart=%h/.cargo/bin/landrop-cli receive --yes --quiet --port 41234
Restart=on-failure

[Install]
WantedBy=default.target
```

```console
$ systemctl --user enable --now landrop-cli
```

Windows 上 `nssm` 或计划任务都可以；要点是传 `--quiet` 和**显式的 `--port`**。

## 故障排查

### `discover` 什么也找不到

- 确认对方设备**已打开应用并处于可接收状态**。
- 部分网络会阻断客户端之间的组播(访客 Wi-Fi、开了客户端隔离的网络)。**如果直连地址可用，
  那就是组播被挡了。**
- 对方必须和你在同一子网。组播默认不跨路由器。
- 知道地址就直接跳过发现：`--to 192.168.1.42:44769`。

### 找到了设备，但收不到东西

- 传输列表只显示**正在接收**的设备。通告端口为 `0` 的设备说明它**关掉了接收**。
- 发送端**等待一段时间后报超时**(而不是立刻被拒绝),通常是**接收端**的防火墙在拦截。
  `receive` 启动时如果发现缺少规则，会打印需要添加的那条命令。

### 第一次给新设备发送，还没传数据就停了

需要有人在接收设备上点接受。没人点的话，会在 `--reply-timeout` 秒后放弃。

### `receive` 用不了 52637 端口

桌面版 App 运行期间会占着该端口，所以同机同时运行时**发现功能不可用**。传输不受影响：
它们用另一个端口，而 `--to host:port` 完全跳过发现。遇到这种情况接收端会给出警告，
并告诉你该用哪个地址。

### 桌面版 App 同时也在运行时发送文件

两者是**独立的安装、独立的身份**。各自维护自己的配置目录和信任列表，所以信任了其中
一个的设备**不会**自动信任另一个。
