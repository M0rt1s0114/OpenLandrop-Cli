# OpenLandrop-Cli

[![CI](https://github.com/M0rt1s0114/OpenLandrop-Cli/actions/workflows/ci.yml/badge.svg)](https://github.com/M0rt1s0114/OpenLandrop-Cli/actions/workflows/ci.yml)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org)

[English](README.md) | **简体中文**

一个使用Rust编写的，基于**LANDrop v2** 局域网传输协议的开源客户端应用程序。

它实现的是与 LANDrop v2 相同的线路协议，因此可以与已有的 LANDrop v2 设备互操作，**不需要**修改、重装或重新配对任何设备。

> **关于名称。** 本项目是独立项目，与 LANDrop 的所有者无隶属关系，亦未获其背书。 见[商标](#商标)。

---

## 状态

| 方面 | 状态 |
|---|---|
| Windows | 已构建 release 二进制，并与 LANDrop v2.7.2 桌面端实测互通 |
| Linux | CI 中构建 release 二进制并自检 |
| 设备发现 | 用第二套实现双向验证通过 |
| 密钥派生 | 测试套件内含已知答案向量，逐字节核对 |
| 文件传输 | 双向 `SHA-256` 逐字节一致，见[验证](#验证) |
| 测试 | 61 个(`cargo test`),`cargo clippy -- -D warnings` 无警告，`cargo fmt` 无差异 |

## 环境要求

- Rust 1.88 或更高(edition 2024,使用 let-chains)
- 不需要 C 工具链，不需要系统加密库。所有依赖均为纯 Rust, 因此 Windows 和 Linux 上都只需 `cargo build` 一步。

## 构建

```console
cargo build --release        # 产物在 target/release/landrop-cli[.exe]
cargo test                   # 61 个测试
cargo run -- selftest        # 端到端自检，不需要任何对端
```

两个平台都可以原生构建，除 Rust 工具链外不需要任何东西。

## 用法

```console
# 列出局域网设备
landrop-cli discover

# 发送文件或目录(不带 --to 时显示交互式选择菜单)
landrop-cli send ./report.pdf ~/Pictures/holiday/

# 指定目标：设备名 / 公钥 / host:port 三种都支持
landrop-cli send f.zip --to build-server
landrop-cli send f.zip --to AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u
landrop-cli send f.zip --to 192.168.1.42:44769

# 从标准输入读取路径列表
find . -name '*.log' | landrop-cli send --stdin

# 接收文件
landrop-cli receive --dir ~/Downloads

# 发送一段文本
landrop-cli text "build finished" --to build-server
```

### 命令

| 命令 | 作用 |
|---|---|
| `discover` | 通过 UDP 组播发现 LANDrop v2 设备 |
| `send <路径...>` | 发送文件或目录(目录递归，保留相对结构) |
| `receive` | 作为接收端运行，供其他设备发送 |
| `text <文本>` | 发送一段短文本 |
| `devices` | 查看或忘记此前记住的设备 |
| `trusted` | 管理免确认接收的发送方 |
| `identity` | 显示本客户端的身份、公钥与配置路径 |
| `selftest` | 本机回环端到端自检，不需要对端 |

完整参数见 `landrop-cli <命令> --help`。

## 非交互用法

`--json` 会把结构化结果写到 stdout,所有面向人的输出转到 stderr, 并隐含非交互模式(不会弹出菜单或确认提示)。失败时退出码非 0, stdout 仍然是一段合法 JSON。

```console
$ landrop-cli send ./a.bin --to 192.168.1.42:44769 --json
{
  "status": "ok",
  "target": {
    "name": "build-server", "type": "linux",
    "address": "192.168.1.42", "port": 44769,
    "public_key": "AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u"
  },
  "verification_code": "844443",
  "files": [
    { "filename": "a.bin", "size": 8388608,
      "last_modified": 1759234000, "permissions": "644" }
  ],
  "total_bytes": 8388608,
  "sent_bytes": 8388608,
  "duration_seconds": 1.22,
  "throughput_bytes_per_second": 6876616
}
```

## 身份与信任

本客户端的身份是一对长期**密钥**,首次运行时生成并保存在：

| 平台 | 位置 |
|---|---|
| Windows | `%APPDATA%\landrop-cli\identity.json` |
| Linux | `~/.config/landrop-cli/identity.json` |
| 任意 | 用 `--config-dir <DIR>` 或 `LANDROP_CLI_CONFIG_DIR` 覆盖 |

公钥是对方信任列表中识别本客户端的依据，因此必须保持稳定； 每次运行都换新密钥会导致对方每次都把它当成新设备。

本客户端使用**自己的**身份和**自己的**信任列表，不与这台机器上的任何其它程序共用。代价是首次向某台设备 发送时，那台设备会弹出确认框：

1. 设备上会显示一个 6 位验证码，必须与 CLI 打印的一致， 否则说明连接被中间人拦截。
2. 确认一次即可完成传输。
3. 之后设备会提供一次性的 **Trust** 操作。确认后本客户端的公钥会被写入 该设备的信任列表，此后传输不再提示。

### 接收端信任列表

`receive` 在配置目录的 `settings.json` 中维护自己的信任列表：

| 发送方 | `receive` 的行为 |
|---|---|
| 在信任列表中 | 静默接收 |
| 不在列表中，有终端 | 打印文件清单与验证码，询问是否接收，随后询问是否记住该发送方 |
| 不在列表中，无终端(管道或服务) | **拒绝** |
| 不在列表中，带 `--yes` | 接收，但**不修改**信任列表 |

`--yes` 与信任列表是两件事：前者是"这一次收下",后者是"以后这个发送方都收"。 信任列表只会在显式操作时改变。

被拒绝时，发送方的公钥和授权命令会写到 stderr:

```
warning: declining dev-laptop (A0xBTkRST1AtRVhBTVBMRS1LRVktVFdPLi4uLi4uLi4u):
  not in the trust list and no terminal to confirm.
  to allow it:  landrop-cli trusted --add A0xBTkRST1AtRVhBTVBMRS1LRVktVFdPLi4uLi4uLi4u
```

### 配置文件

配置目录下的 `settings.json`。所有字段都是可选的，未知字段会被忽略， 因此新版本增加字段不会导致旧版本读取失败。

```json
{
  "device_name": null,
  "download_dir": null,
  "listening_port": 0,
  "known_devices": [],
  "trusted_devices": [
    {
      "name": "build-server",
      "public_key": "AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u",
      "added_at": 1790769028
    }
  ]
}
```

## 作为后台服务运行

`receive` 本身就是一个长期运行的监听进程。配合信任列表， 来自已授权设备的传输无需任何交互即可接收：

```console
landrop-cli receive --quiet --dir /srv/landrop/inbox
```

`--quiet` 会关闭启动横幅和进度条(否则会向服务日志写入 ANSI 转义序列), 但保留传输日志。`Ctrl-C` 会执行干净的关闭流程。systemd 与 Windows 计划任务的 配置见 [`docs/USAGE.zh-CN.md`](docs/USAGE.zh-CN.md)。

> **已知限制。** 设备发现使用 UDP 端口 **52637**,而 LANDrop 桌面端也会绑定该端口。只要桌面端在同一台机器上运行，本客户端就无法绑定该端口，也就无法广播自己：它不会出现在其他设备的发现结果中。TCP 监听不受影响。变通办法见 [`docs/USAGE.zh-CN.md`](docs/USAGE.zh-CN.md#receive-用不了-52637-端口)。

## 验证

下表中的测量针对 **LANDrop v2.7.2**,除特别说明外均走回环 TCP。

| 检查项 | 方法 | 结果 |
|---|---|---|
| CLI → 桌面端，单文件 | 8 MiB / 24 MiB / 64 MiB | 落盘 `SHA-256` 一致 |
| CLI → 桌面端，目录 | 含非 ASCII 名、emoji 名、带空格名、零字节文件与嵌套子目录的目录树 | 6 个文件 `SHA-256` 全部一致，结构保留 |
| CLI → 桌面端，大量小文件 | 500 × 32 KiB | 完整 |
| CLI → 桌面端，文本 | 文本消息往返 | 桌面端已确认 |
| 交互提示 | 接收端的 Accept + Trust 提示 | 人工确认，双方验证码一致 |
| 另一发送方 → CLI 接收端 | 3 MiB,来自第二套实现 | `SHA-256` 一致 |
| 信任列表，未授权发送方 | 非交互接收端 | 拒绝，未写入任何文件 |
| 信任列表，已授权发送方 | `trusted --add` 之后 | 静默接收，`SHA-256` 一致 |
| 设备发现，双向 | 两套实现 | 两个方向都能发现设备 |
| 密钥派生 | 固定输入，已知答案向量 | 逐字节一致 |

**尚未验证。** 以下内容没有实测过，不应假定成立：

- 桌面端向**同一台机器上**的本客户端接收端发送(受上文 UDP 52637 限制阻塞)。
- 真实局域网或 Wi-Fi 下的吞吐。下表全部是回环数据，网络不是瓶颈。
- Windows 以外的平台，以及 v2 以外的 LANDrop 版本。
- 大于 4 GiB 的文件。

## 性能

回环 TCP,同机,CLI 发送到 LANDrop v2.7.2 桌面端：

| 场景 | 吞吐 |
|---|---|
| 8 MiB 单文件 | 约 100 MiB/s |
| 64 MiB 单文件 | 约 86 MiB/s |
| 500 × 32 KiB 小文件 | 约 10 MiB/s |
| CLI → CLI(两端都是 Rust) | 约 300 MiB/s |
| `selftest`(纯内存) | 约 410 MiB/s |

从这些数字可以看出两点：

- 大文件吞吐的瓶颈在接收端，不在本客户端。真实网络中瓶颈通常先出现在链路上。
- 大量小文件明显更慢：接收端每个文件要多做约六次系统调用 (`open`、`write`、`close`、`stat`、`utimes`、`chmod`),约 3 ms/文件。

## 限制

1. **仅支持局域网。** 互联网 WebRTC 中继传输未实现。
2. **UDP 52637 是独占的**,如上所述。
3. **同机上的 CLI 与桌面端无法互相发现。** 本客户端按公钥而不是按地址过滤发现，因此同机上两个本客户端实例可以互相发现；它看不到桌面端，桌面端也看不到它。
4. **文件名清洗是本客户端自己的策略。** 收到含路径穿越(`..`)、绝对路径或 `:` 的文件名时，会在接受请求之前就拒绝。经本客户端落盘的文件不会跑到下载目录之外。
5. **声明的大小是权威值。** 传输过程中变短的文件会让本次传输报错中止，而不是让接收端一直等着永远来不了的字节。

## 文档

| 文档 | English | 简体中文 |
|---|---|---|
| 项目说明 | [README.md](README.md) | [README.zh-CN.md](README.zh-CN.md) |
| 用法参考 | [docs/USAGE.md](docs/USAGE.md) | [docs/USAGE.zh-CN.md](docs/USAGE.zh-CN.md) |

**[`skills/landrop-cli`](skills/landrop-cli/SKILL.md)** 是一份 Agent 如何驱动本 CLI 的 Skill，遵循跨工具的 `.agents/skills` 约定，任何读取该约定的 harness 都可以直接使用。

## 参与贡献

见 [`CONTRIBUTING.md`](CONTRIBUTING.md)。提交信息遵循 [Conventional Commits](https://www.conventionalcommits.org/)。

## 致谢

- **[LANDrop](https://landrop.app)** —— 感谢原作者为我们提供的优秀应用，本项目初衷是提供一个可以供无桌面环境 Linux 环境下使用的 Landrop 程序。如有侵权问题，请直接发 issue 联系我删除仓库。
- **[DeepSeek](https://www.deepseek.com)** —— 感谢 DeepSeek-V4.1 Flash 在我此项工作中的重大帮助与贡献，极大的增加了我的效率。

## 许可证

GPL-3.0-or-later,见 [`LICENSE`](LICENSE)。

## 商标

"LANDrop" 及相关标识归其各自所有者所有。 本项目是与其 LANDrop v2 兼容的独立实现，与上述所有者无隶属关系，亦未获其背书。
