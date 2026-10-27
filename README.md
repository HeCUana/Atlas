# Atlas

> 用 Rust 编写的 Minecraft 多服务器管理工具。本体保持精简，但多服务器管理能力直接具备；业务功能通过 Atlas Plugin 调用 Atlas 接口扩展。

| 项目 | 说明 |
| --- | --- |
| 语言 / 运行时 | Rust，异步运行时基于 tokio |
| 形态 | 单二进制，CLI + TUI；Minecraft 服务端以**子进程**方式被托管 |
| 许可证 | Apache-2.0 |
| 核心定位 | 多服务器的开服 / 关服 / 重启、日志解析、文件与命令管理、插件扩展 |
| 当前重点 | **日志识别与解析**，并将解析结果转换为供插件调用的接口 |

### 文档索引

| 文档 | 定位 | 主要读者 |
| --- | --- | --- |
| `README.md`（本文） | 项目介绍 + 技术设计：架构、数据流、技术选型、风险与取舍 | 开发者、项目评审人 |
| `docs/功能文档.md` | 纯功能说明：能力清单、实现状态、当前范围与未来规划 | 服务器管理员、产品/评审 |

---

## 目录

1. [设计原则](#1-设计原则)
2. [核心能力总览](#2-核心能力总览)
3. [运行时架构](#3-运行时架构)
4. [日志识别与解析（当前重点）](#4-日志识别与解析当前重点)
5. [事件系统与命令树](#5-事件系统与命令树)
6. [插件系统](#6-插件系统)
7. [权限模型](#7-权限模型)
8. [Rust 技术选型](#8-rust-技术选型)
9. [当前范围与未来规划](#9-当前范围与未来规划)
10. [风险登记册](#10-风险登记册)
11. [术语表](#11-术语表)
12. [待确认信息](#12-待确认信息)

---

## 1. 设计原则

Atlas 的边界划得很窄，但每一条都做成"开箱即用"：

- **本体精简**：Atlas 不试图成为 Minecraft 服务端生态的全能管理端，也不内置任何具体业务逻辑（榜单、经济、任务、备份策略等一律外置为插件）。
- **多服务器能力直给**：与"只提供一个插件 SDK、其余靠生态补齐"的路线不同，Atlas 自身直接提供多服务器管理能力——子进程托管、批量开关服、SFTP 文件管理、日志解析、命令执行、远程 TUI。
- **扩展靠插件**：所有业务功能通过 Atlas Plugin 调用 Atlas 已有的接口实现，Atlas 只需要维护接口的稳定性，而不是跟着业务需求演进。
- **服务端不被绑架**：Minecraft 服务端是独立子进程，Atlas 的崩溃、重启、热重载都**不需要关服**。这条约束是后面很多设计取舍的根源。

---

## 2. 核心能力总览

| 能力域 | 说明 | 状态 |
| --- | --- | --- |
| 子进程托管 | 将 Minecraft 服务端作为子进程启动，捕获其标准输入/输出/错误 | 已有 |
| TUI 控制台 | 通过 TUI 单独控制每一个子进程，并可控制主进程的守护进程 | 已有 |
| 多服务器生命周期 | 批量开服、关服、重启等开关操作 | 已有 |
| SFTP 远程文件管理 | 远程管理 Atlas 所管理的每个服务器中的文件 | 已有 |
| 日志识别与解析 | 识别多种服务端日志格式，转换为结构化事件 | **当前重点** |
| 事件系统 | 作为日志能力的对外接口，插件订阅事件流 | 已有 |
| 命令树 | 为插件提供标准命令系统，衔接服务端命令与游戏内内置命令 | 已有 |
| 终端命令执行 | 在玩家当前所在服务器的终端执行命令，并采集返回信息 | 已有 |
| 插件热重载 | 不关闭服务器的情况下更新、启用或禁用插件 | 已有（存在取舍，见 §6.4） |
| 多形态插件 | 单文件插件、打包插件（`.atlas` / `.rs`）、文件夹插件 | 已有 |
| 权限管理 | 配套权限系统，保证并非所有人发出的信息都会被执行 | 已有 |
| 远程 TUI | 从远端连接并操作 Atlas | 已有 |
| Mod 数据源 | 通过 Mod 采集游戏内信息，或经 Mod 接口回传信息 | 规划中 |

### 2.1 代码结构与构建

Atlas 是一个 Cargo workspace，`atlas-core` 是所有模块共享的契约基座：

```
crates/
  atlas-core/    共享类型层：事件、命令树、权限模型、配置
  atlas-parser/  日志识别与解析（当前重点）
  atlas-server/  子进程托管、生命周期、终端命令执行
  atlas-plugin/  插件清单、发现、注册表、热重载编排
  atlas-sftp/    远程文件管理
  atlas-tui/     终端界面
  atlas-cli/     二进制入口（atlas）
```

**依赖方向是单向的**：`atlas-cli` 依赖全部模块，模块之间互不依赖，只通过 `atlas-core` 的类型协作。

```bash
cargo build --workspace        # 构建全部
cargo test  --workspace        # 跑全部测试
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p atlas-cli -- --help
```

配置示例见 [`atlas.toml.example`](atlas.toml.example)。工程约定见 [`AGENTS.md`](AGENTS.md)。

> **Windows 构建提示**：`russh` 的默认加密后端 `aws-lc-rs` 在 Windows x86_64 上默认需要本机安装 NASM 汇编器。仓库内的 [`.cargo/config.toml`](.cargo/config.toml) 已设置 `AWS_LC_SYS_PREBUILT_NASM=1`，改用 `aws-lc-sys` 自带的预编译目标文件，**无需安装任何外部工具**。

### 2.2 首次运行：自动生成配置与目录

新克隆的仓库里没有配置也没有目录。Atlas **不会**因为这些缺失而失败或静默降级——它会自己补齐：

```console
$ atlas init
已生成配置文件: atlas.toml
已创建服务器根目录: servers
已创建目录: plugins
已创建目录: logs

下一步：编辑 atlas.toml 添加服务器，然后用 `atlas start` 启动。
```

- **`atlas init`**：显式脚手架。生成 `atlas.toml` 模板（服务器段落以注释给出，取消注释即可启用）、`servers/`、`plugins/`、日志目录。`--force` 可覆盖已有配置。
- **任何子命令首次运行**：若 `atlas.toml` 不存在，会自动生成同一份模板并建好目录，然后继续执行（不会因为缺配置而只跑一个空默认值）。
- **每次运行**：配置里引用到的目录若不存在会被补建——服务端的 `workdir` 缺失会导致进程直接启动失败，插件目录缺失会让插件发现静默返回空列表，这两处必须存在。
- **幂等**：已存在的配置**不会**被覆盖（除非显式 `--force`），已存在的目录直接跳过。

相对路径一律以 **Atlas 进程的工作目录**为基准解析。

### 2.3 SSH 客户端密钥（`atlas keygen`）

Atlas 用 SSH 客户端密钥连接远端服务器做 SFTP 文件管理，配置文件里的 `key_path` 指向一个**本地私钥**。

```console
$ atlas keygen
已生成 SSH 客户端密钥对：
  私钥: keys/id_ed25519
  公钥: keys/id_ed25519.pub
  格式: openssh
  指纹: SHA256:kJA+fxo6/RpZE7iILwkzH4blLCRWqL7yT8N/AmTzGdg

下一步：把下面这行公钥追加到远端服务器的 ~/.ssh/authorized_keys：
  ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIL4IcFQ27/rFcMcd/814WLqvLfwlUnzw17CgoJDkohQz atlas

然后在 atlas.toml 的 [sftp] 段配置：
  key_path = "keys/id_ed25519"
```

#### 为什么默认没有 `.pem`

`.pem` 是**容器编码**的名字，不是密钥类型。别被文件名带偏——一个叫 `.pem` 的文件，内容未必是标准 PEM。

| 格式 | 私钥内容 | 私钥文件名 | 公钥文件名 | 加密 |
| --- | --- | --- | --- | --- |
| `openssh`（默认） | `-----BEGIN OPENSSH PRIVATE KEY-----` | `keys/id_ed25519` | `keys/id_ed25519.pub` | 不支持 |
| `pkcs8` | `-----BEGIN PRIVATE KEY-----` | `keys/id_ed25519.pem` | `keys/id_ed25519.pub` | 支持 |

公钥文件名与私钥格式无关（公钥内容本来就是单一格式），始终是 `<名字>.pub`。

**默认选 OpenSSH 格式**：那是 `ssh-keygen` 的默认产物，也是 `ssh` / `scp` / `sftp` / `~/.ssh/config` 的 `IdentityFile` 全都认的约定。把 OpenSSH 格式的文件叫 `.pem` 反而误导——它的内容不是标准 PEM。

需要标准 PKCS#8 PEM（部分工具只认这个）时：

```console
$ atlas keygen --format pkcs8                          # keys/id_ed25519.pem
$ atlas keygen --format pkcs8 --passphrase <口令>        # 加密私钥
```

加密私钥的 PBKDF2-SHA256 迭代次数取 **600,000**（OWASP 推荐值）——OpenSSH 的历史默认值只有 16，对今天的算力毫无意义。连接时把口令填进 `[sftp] key_passphrase`。

#### 其余约定

- **为什么不自动生成**：私钥是凭据，生成它属于安全敏感动作，不该在启动时悄悄发生；而且公钥无论如何都要人工装到远端的 `authorized_keys`，跑不掉那一步。所以这里是**显式命令**，由你决定何时生成。
- 私钥为 OpenSSH 格式时与 `ssh-keygen` 生成的互相兼容；Unix 下权限收紧为 `0600`，**Windows 下自动用 `icacls` 把 ACL 收紧为「Administrators + SYSTEM」**——否则既不安全（同机其它账户可读），标准 `ssh`/`scp` 也会直接拒用该私钥（"UNPROTECTED PRIVATE KEY FILE"）。
- 已存在的私钥**不会被覆盖**（覆盖 = 销毁旧凭据），需要覆盖请显式加 `--force`。
- `atlas init` 会创建 `keys/` 目录；配置里若填了 `[sftp] key_path`，其父目录也会被自动补建。

### 2.4 远程文件管理（`atlas sftp`）

Atlas 通过 SFTP 管理所托管服务器中的文件。**Atlas 是 SFTP 客户端**——`host` 填的是**远端**服务器地址。

**两种用法**：

```console
$ atlas sftp                      # 交互式会话（不带子命令）
Atlas SFTP 交互会话 — 远端根目录: /
输入 help 查看可用命令，quit 退出。
sftp:/> ls /
d            0  /C:
d            0  /D:
sftp:/> cd C:/Users/Administrator/Desktop
sftp:/C:/Users/Administrator/Desktop> pwd
远端当前目录: /C:/Users/Administrator/Desktop
sftp:/C:/Users/Administrator/Desktop> quit
已退出会话。
```

交互会话支持的命令：

| 命令 | 说明 |
| --- | --- |
| `ls [路径]` | 列出远端目录（默认当前目录） |
| `stat <路径>` | 查看远端文件/目录信息 |
| `cd <路径>` / `pwd` | 切换 / 显示远端当前目录 |
| `lcd <路径>` / `lpwd` | 切换 / 显示**本地**当前目录 |
| `get <远端> [本地]` | 下载（省略本地名则用远端文件名） |
| `put <本地> [远端]` | 上传（省略远端名则放进当前目录） |
| `rm <远端>` / `rmdir <远端>` | 删除文件 / 删除目录（须为空） |
| `!<命令>` | 执行本地 shell 命令 |
| `help` / `quit` | 帮助 / 退出 |

**一次性模式**（带子命令，便于脚本化）：

```console
$ atlas sftp ls /
$ atlas sftp put ./config.toml /home/mc/config.toml
$ atlas sftp get /home/mc/latest.log ./latest.log
$ atlas sftp mkdir /home/mc/backups
$ atlas sftp rm /home/mc/old.log
$ atlas sftp rmdir /home/mc/old-backups
```

先在 `atlas.toml` 里配好 `[sftp]` 段：

```toml
[sftp]
host = "192.168.1.10"        # 远端服务器地址，不是本机
port = 22
username = "mc"
key_path = "keys/id_ed25519" # `atlas keygen` 生成的私钥
root = "/srv/minecraft"        # 可选：把 SFTP 会话限制在这个目录之下
```

#### 可配置的远程根目录

`root` 决定 SFTP 会话**从哪里开始**，同时也是**访问边界**：

- 省略 `root` → 会话从文件系统根开始（Linux 上是 `/`，Windows 上是盘根）；
- 设置 `root` → 会话从该目录开始，并且**无法越出**（`..` 会被拒绝）。

```console
$ atlas sftp
远端根目录: /C:/Users/Administrator/Desktop
Atlas SFTP 交互会话 — 远端根目录: /C:/Users/Administrator/Desktop
sftp:/C:/Users/Administrator/Desktop> cd 02_开发与AI
sftp:/C:/Users/Administrator/Desktop/02_开发与AI> stat /C:/Windows
错误: /C:/Windows 越出根目录 /C:/Users/Administrator/Desktop
```

路径规则：

| 输入 | 含义 |
| --- | --- |
| `plugins` | 相对**当前目录** |
| `/` | **根目录**本身 |
| `/srv/mc/x` | 绝对路径，必须落在 `root` 之下，否则拒绝 |
| `..` | 上级目录，但不能越过 `root` |

Windows 路径会被自动转成 SFTP 的绝对 POSIX 形式：`C:\Users\Administrator\Desktop` → `/C:/Users/Administrator/Desktop`（与 sshd 返回的路径形式一致，第三方工具看到的是同一种路径）。

> **第三方客户端（FileZilla 等）不受这个配置约束**——它们连的是 sshd，落地目录由 sshd 决定。要让 FileZilla 也从指定目录开始，在「站点管理器 → 高级 → 默认远程目录」填同样的路径（那只是**起始目录**，仍可往上跳）。

```toml
[sftp]
host = "192.168.1.10"        # 远端服务器地址，不是本机
port = 22
username = "mc"
key_path = "keys/id_ed25519" # `atlas keygen` 生成的私钥
```

**连不上时的排查顺序**（`ECONNREFUSED` / `os error 10061`）：

1. `host` 填的是不是**远端**地址——填 `127.0.0.1` 就是连本机，本机没装 SSH 服务时必然被拒；
2. 目标主机上 `sshd` 有没有在跑、`port` 对不对（`systemctl status sshd` / `netstat -ano | findstr :22`）；
3. 中间有没有防火墙拦截。

命令会在连接类失败时直接给出这条提示，不用自己猜：

```
Error: 连接 SFTP 服务器 127.0.0.1:22 失败：… 由于目标计算机积极拒绝，无法连接。 (os error 10061)
提示：确认该主机上 SSH 服务在监听（sshd 未启动 / 端口不对 / 被防火墙拦截）。
```

**主机密钥校验默认开启**：未知主机一律拒绝（防中间人）。首次连新主机时，二选一——先把主机公钥写进 known_hosts，或临时用 `--accept-unknown-host`（会记录审计日志）。

#### 服务端：目标主机上要先有 SSH 服务

`ECONNREFUSED` / `os error 10061` 的含义是**目标端口上没人接受连接**——不是认证失败，也不是超时。对应到 Atlas：

| 场景 | 要做的事 |
| --- | --- |
| 连的是**远端** MC 服务器 | 在那台机器上装并启动 SSH 服务（Linux：`apt install openssh-server` / `dnf install openssh-server` → `systemctl enable --now sshd`） |
| 连的是**本机**（Windows） | 装 OpenSSH Server：`Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0` → `Start-Service sshd` → `Set-Service sshd -StartupType Automatic` → 放行防火墙 `New-NetFirewallRule -Name sshd -DisplayName 'OpenSSH Server (sshd)' -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22` |
| 连的是**本机**（Linux） | `apt install openssh-server && systemctl enable --now sshd` |

#### Windows 服务端的公钥放哪

Windows OpenSSH 对**管理员账户**有专门规则（`sshd_config` 里的 `Match Group administrators`）——公钥不放 `~/.ssh/authorized_keys`，而是放：

```
C:\ProgramData\ssh\administrators_authorized_keys
```

并且 ACL 必须收紧到只有 Administrators 与 SYSTEM（否则 sshd 会拒绝加载）：

```powershell
icacls "C:\ProgramData\ssh\administrators_authorized_keys" /inheritance:r
icacls "C:\ProgramData\ssh\administrators_authorized_keys" /grant '*S-1-5-32-544:F' '*S-1-5-18:F'
```

用 SID 而不是组名，是为了在非英文 Windows 上也能跑通。

#### 客户端私钥的 ACL 也要收紧

同一类问题在**客户端侧**同样存在：私钥默认继承所在目录的 ACL，Windows 自带的 `ssh`/`scp` 见到过宽的 ACL 会**直接拒绝使用该私钥**：

```
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@
@         WARNING: UNPROTECTED PRIVATE KEY FILE!         @
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@
Permissions for 'keys\id_ed25519' are too open.
```

`atlas keygen` 现在**自动**把私钥及其所在目录的 ACL 收紧为「Administrators + SYSTEM」（Windows 下用 `icacls`，Unix 下设为 `0600`）。如果私钥是别处生成的，手动执行：

```powershell
icacls "keys\id_ed25519" /inheritance:r
icacls "keys\id_ed25519" /grant '*S-1-5-32-544:F' '*S-1-5-18:F'
icacls "keys" /inheritance:r
icacls "keys" /grant '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F'
```

> 目录的授权**必须带 `(OI)(CI)`**（可继承）。`icacls /grant` 加的 ACE 默认只作用于该对象自身，目录不带继承标志的话，之后在目录里新建的文件会继承到**空 ACL**——文件存在却"访问被拒绝"。

### 2.5 Atlas 作为 SFTP 服务端（`atlas serve`）

这是与 `atlas sftp`（客户端）**方向相反**的能力：**Atlas 当服务端**，远端运维电脑用 SFTP 连进来，管理 Atlas 所托管的服务器的文件。

```console
$ atlas serve
SFTP 服务端启动中……
  监听:      0.0.0.0:2222
  主机私钥:  keys/host_ed25519
  授权公钥:  keys/authorized_keys
  共享目录:
    - survival         → servers/survival
    - creative         → servers/creative

远端用 SFTP 客户端连接：sftp://<用户名>@<Atlas主机>:2222
```

客户端看到的是一棵**虚拟树**，每个受管服务器一个顶层目录：

```console
$ sftp -P 2222 atlas@atlas-host
sftp> ls
creative  survival
sftp> ls /survival
/survival/README.txt      /survival/world/
sftp> get /survival/world/level.dat ./level.dat
```

**准备步骤**：

```bash
# 1. 生成服务端主机密钥（远端客户端首次连接时校验的就是它）
atlas keygen --path keys/host_ed25519

# 2. 把远端电脑的**客户端公钥**写进 keys/authorized_keys（一行一个，形如
#    `ssh-ed25519 AAAA... 备注`）

# 3. 把配置里 [sftp_server] 的 enabled 改成 true
```

**安全边界**：

- **只接受公钥认证**，口令认证一律拒绝；客户端公钥不在 `authorized_keys` 里就登不进来；
- 客户端路径**必须落在共享目录之内**——`..` 无法越出，`ls /C:/Windows` 会被服务端拒绝；
- 共享可标记只读（`read_only = true`），拒绝写入、删除、改名与建目录；
- 端口默认 **2222** 而不是 22——避免和本机 OpenSSH 服务端抢端口；
- 认证失败固定耗时（防时序探测）。

> 共享目录默认从 `[[servers]]` 自动派生（服务器 `id` → 虚拟目录名，`workdir` → 真实目录）。要额外共享别的目录，用 `[[sftp_server.share]]`。
>
> `atlas serve` 与 `atlas start` 是**两个独立进程**：前者只提供文件通道（不启 Minecraft 服务端），后者只托管 Minecraft 服务端（不提供文件通道）。两者可以同时跑。

### 2.6 本机 SSH 服务端（`atlas sshd`）

SFTP 连本机时，前提是本机有 SSH 服务在跑。`atlas sshd` 负责这件事，并附带一份**就绪检查**：

```console
$ atlas sshd status
本机 SSH 服务端（sshd）状态
  服务已安装    ✓
  服务正在运行  ✓
  开机自启      Automatic
  22 端口监听   ✓
  客户端私钥    ✓
  公钥已授权    ✓
    ↳ C:\ProgramData\ssh\administrators_authorized_keys
  known_hosts   ✓

SFTP 就绪 ✓
```

| 子命令 | 说明 |
| --- | --- |
| `atlas sshd status` | 查询服务状态 + SFTP 就绪检查（服务、端口、私钥、公钥、known_hosts、**私钥污染**） |
| `atlas sshd start` | 启动服务并设为开机自启 |
| `atlas sshd stop` | 停止服务 |
| `atlas sshd install` | 安装 SSH 服务端（Windows 装 OpenSSH Server 可选功能） |
| `atlas sshd authorize` | 把客户端公钥装进服务端的 `authorized_keys`（并收紧 ACL） |

> **安全约定**：启动 SSH 服务会对外开放 22 端口，属于安全敏感操作。所以这里只有**显式命令**，**绝不在 Atlas 启动时自动开启**。
>
> `authorize` 从**私钥**推导公钥（不依赖可能陈旧的 `.pub` 文件），并且**拒绝**往混有私钥的文件里写——那只会掩盖问题。曾经写进 `authorized_keys` 的私钥应视为**已泄漏**，需要作废并重新生成。
>
> `status` 也会主动检测 `authorized_keys` 里是否混入了私钥（带 PEM 头的完整私钥，或只剩 base64 正文的形态）并高亮告警。

#### 用第三方 SFTP 客户端连本机（FileZilla / WinSCP / PuTTY）

`atlas sshd status` 会打印**主机密钥指纹**——客户端首次连接弹出的"未知主机密钥"指纹必须与它一致，否则可能是中间人。

```
$ atlas sshd status
  ...
  主机密钥指纹  SHA256:NFrOZeHA1s6tSe12w68l7ZTfWhi7+sIrMpDC3YqgEZ0
```

客户端参数（以 FileZilla 的「站点管理器」为例）：

| 项 | 值 | 常见踩坑 |
| --- | --- | --- |
| 协议 | **SFTP - SSH File Transfer Protocol** | 选成 FTP 会连 21 端口，直接被拒 |
| 主机 | `127.0.0.1` | **别填 `localhost`**（可能解析成 ::1） |
| 端口 | `22` | |
| 用户名 | `Administrator` | 必须填**真实存在的 Windows 账户**；填不存在的会看到服务端日志里的 `Invalid user xxx` |
| 登录类型 | **密钥文件**（Key file） | 选成"智能卡"会走 PKCS#11 路径，根本不读普通私钥 |
| 私钥文件 | `C:\Users\<你>\Desktop\Atlas\keys\id_ed25519` | **绝不要选 `C:\ProgramData\ssh\ssh_host_ed25519_key`**——那是服务器的身份密钥，不是给客户端用的 |
| 字符集 | UTF-8 | 中文文件名乱码时改这里 |

> **Windows 管理员账户的公钥位置**：`C:\ProgramData\ssh\administrators_authorized_keys`（`sshd_config` 的 `Match Group administrators` 规则），不是 `~/.ssh/authorized_keys`。`atlas sshd authorize` 会自动装到正确位置并收紧 ACL。
>
> **私钥格式**：FileZilla 3.60+ 原生支持 OpenSSH 格式私钥（`-----BEGIN OPENSSH PRIVATE KEY-----`），无需转成 PuTTY 的 `.ppk`。

### 2.7 运行方式

`atlas start` 是**前台守护模式**：启动目标服务器后接管它们，按 `[服务器ID] ` 前缀实时输出各服务器的控制台行。

```console
$ atlas start
已启动 1 台服务器：
  - survival  (生存服)

以下为各服务器的实时输出（Ctrl+C 将优雅停止全部服务器）：
[survival] [12:34:56] [Server thread/INFO]: Done (3.214s)! For help, type "help"
```

几条不会让人"看着像卡死"的保证：

- **没有可启动的服务器时不会挂住**：配置为空就打印引导并退出；配置里有服务器但都没开 `auto_start`，则列出可用的服务器 ID 并提示 `atlas start <服务器ID>`。
- **服务端意外崩溃时立刻提示**：`[survival] 进程已退出，当前状态：已失败`——而不是让日志悄悄停住，让人分不清是"崩了"还是"卡了"。
- **Ctrl+C 是优雅停止**：先把停止命令投递给子进程、等待其自行退出，超时才强制终止。直接 kill 会丢服务端的世界存档。

> **不要同时跑 `atlas start` 和 `atlas tui`**。Atlas 的每次命令行调用都是**独立进程**——`atlas tui` 自己托管服务器，`atlas start` 是无界面的守护模式，同时开会让两边都去拉起同一批服务端、导致端口冲突。二选一即可。
>
> `atlas stop` 停的是**本进程托管**的实例。若目标服务器不由本进程启动，命令会明确区分「配置里压根没这台服务器」与「配置里有、但不由本进程托管」两种情况，而不是笼统报错。

---

## 3. 运行时架构

### 3.1 进程模型

```
┌─────────────────────────────────────────────┐
│  Atlas 进程                                  │
│                                             │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  │
│  │ supervisor│  │ 事件总线 │  │ 命令树   │  │
│  │ 守护进程  │  │ broadcast│  │  路由    │  │
│  └────┬─────┘  └────▲─────┘  └────▲─────┘  │
│       │             │             │         │
│       │ stdin/out   │             │         │
│  ┌────┴─────┬───────┴─────────────┴────────┐│
│  │ server A │ │ server B │ │ server C       ││
│  │ (子进程)  │ │ (子进程)  │ │ (子进程)       ││
│  └──────────┘ └──────────┘ └───────────────┘│
└─────────────────────────────────────────────┘
                     ▲
                     │ SFTP / 远程 TUI
              远程运维节点
```

每个 Minecraft 服务端是 Atlas 的直接子进程：

- **输入**：Atlas 通过子进程 stdin 向服务端控制台投递命令（`stop`、`op <玩家>`、插件命令等）。
- **输出**：Atlas 持续读取子进程 stdout/stderr，用于日志解析与 TUI 展示。
- **生命周期**：开服 = spawn；关服 = 向 stdin 写入停止命令并等待进程退出；重启 = 关服 + 开服。

> **设计要点（建议）**：读取子进程输出时，stdout 与 stderr 必须由两个独立任务并发读取，否则高日志量下管道写满会造成互相阻塞死锁。行分隔不能只认 `\n`——Minecraft 服务端控制台的进度条使用 `\r` 覆盖同一行，因此应把 `\r` 与 `\n` 都视为行终止符，并把 `\r` 之间的片段视为"同一行的进度更新"而非独立日志行。
>
> **设计要点（建议）**：子进程的退出必须与 Atlas 主流程解耦。Atlas 自身收到 Ctrl+C 或守护进程重启时，应先把停止命令投递给所有子进程、等待其优雅退出，再退出 Atlas；超时后才升级为强制终止。tokio 官方推荐的优雅关闭组合是 `tokio_util::sync::CancellationToken`（广播取消信号）+ `tokio_util::task::TaskTracker`（等待全部任务收敛）。

### 3.2 TUI 与远程 TUI

TUI 负责：

- 服务器列表视图与单服务器子控制台视图之间切换；
- 对单个子进程输入命令、查看其输出；
- 操作主进程自身的守护进程（重启 Atlas 自身、切换配置等）。

远程 TUI 让运维节点在异地直接进入同一套 TUI。

> **待确认**：远程 TUI 的传输层尚未在项目背景中确定。Rust 生态里已有成熟先例——`russh` 服务端 + PTY 通道承载 `ratatui` 界面（`kty`、`kartoffels`、`sandhole` 等项目均采用此模式），且 `russh` + `russh-sftp` 可以同时满足"远程 TUI"和"SFTP 文件管理"两项需求。因此 **SSH 承载远程 TUI 是候选方案中复用度最高的一种**（建议）。但若最终选型为自研协议，则需额外实现认证、加密、终端尺寸同步与按键编码映射。
>
> 无论哪种传输，远程通道都应复用同一套权限系统，并记录审计日志。

---

## 4. 日志识别与解析（当前重点）

### 4.1 日志格式兼容性

Atlas 需要同时面对两套同源但不完全一致的日志体系：

**A. Log4j2 家族 —— Fabric / Forge / NeoForge**

这三者共用 Log4j2，日志行遵循 Log4j2 的基本模式。典型形态：

```
[12:34:56] [Server thread/INFO]: [minecraft/MinecraftServer]: Done (3.214s)! For help, type "help"
```

模式骨架为 `[%d{HH:mm:ss}] [%t/%level]: [%logger] %msg%n`，其中：

| 占位符 | 含义 |
| --- | --- |
| `%d{HH:mm:ss}` | 时间戳 |
| `%t` | 线程名（如 `Server thread`） |
| `%level` | 日志级别（`TRACE`/`DEBUG`/`INFO`/`WARN`/`ERROR`/`FATAL`） |
| `%logger` | 日志器名称（通常是类名） |
| `%msg` | 日志正文 |

**B. CraftBukkit 家族 —— Paper / Folia / Bukkit**

Paper / Folia / Bukkit 与 A 高度同源，同样使用 Log4j2 作为日志框架（Paper 的 `log4j2.xml` 中 `PatternLayout` 为 `[%d{HH:mm:ss}] [%t/%level]: [%logger] %stripAnsi{%msg}%n%xEx{full}`），区别主要在**交互式控制台**的呈现格式更简洁：

```
[12:34:56 INFO]: Done (3.214s)! For help, type "help"
```

CraftBukkit 系服务端还会在控制台记录玩家的命令执行，典型形态为：

```
<玩家名> issued server command /<命令内容>
```

**这里的关键差异不是日志框架，而是"控制台输出来自哪里"**：

| 部署方式 | Atlas 看到的输出 |
| --- | --- |
| 直接运行 `java -jar server.jar` | 服务端自己的控制台格式 |
| 官方 Docker 镜像（`itzg/minecraft-server` 等） | 镜像定义的 `LOG_CONSOLE_FORMAT` / `LOG_TERMINAL_FORMAT` |
| 第三方面板 / 启动器 | 由面板代理 stdout，格式取决于面板实现 |
| 读取 `logs/latest.log` 文件 | 文件 appender 的格式，可能与控制台不同 |

因此**同一款服务端在不同部署下的控制台行格式并不一致**——这是解析层必须正面处理的问题，不能假定只有一种形态。

### 4.2 解析管线

```
子进程 stdout/stderr
        │  字节流 → 行切分（\r 与 \n 皆为终止符）
        ▼
   ① ANSI 剥离 ──── 日志正文常混有颜色转义序列
        │
        ▼
   ② 格式识别 ──── 按 profile 依次尝试匹配
        │              A: Log4j2 家族
        │              B: CraftBukkit 家族
        │              C: 纯文本降级
        ▼
   ③ 结构化 ────── { server, ts, level, logger, thread, message, raw }
        │
        ▼
   ④ 事件总线 ──── 广播给插件订阅者
```

**建议的匹配策略（建议）**：采用「有结构的正则 + 表驱动 profile」的组合，而不是为每种服务端写一段解析代码。

以 A、B 两族为例，两者的区分点很清晰——A 有第二个方括号字段（`[thread/LEVEL]`），B 没有：

```rust
// A. Log4j2 家族
r"^\[(?P<ts>\d{2}:\d{2}:\d{2})(?:\.\d+)?\]\s*\[(?P<thread>[^\]]+)/(?P<level>[A-Z]+)\]:\s*(?:\[(?P<logger>[^\]]+)\]\s*)?(?P<msg>.*)$"

// B. CraftBukkit 家族
r"^\[(?P<ts>\d{2}:\d{2}:\d{2})(?:\.\d+)?\s+(?P<level>[A-Z]+)\]:\s*(?P<msg>.*)$"
```

两者都不匹配时，走 C：把整行作为 `message`，`level` 记为 `UNKNOWN`，`raw` 保留原文。

**必须坚持的两条底线**：

1. **解析失败不丢行**。任何一行都要以某种形式进入事件流（哪怕是降级事件），否则一次格式漂移就会静默丢掉整段时间的日志。
2. **原始行始终保留**。`raw` 字段让插件和运维人员能回溯未被结构化覆盖的信息，也方便在不改动解析器的前提下新增字段。

### 4.3 可扩展性

服务端的日志格式是可以被用户改写的——Paper 用户可以自定义 `log4j2.xml` 的 `PatternLayout`，Forge / Fabric 用户也可以替换 `log4j2.xml`。

> **建议**：profile 应当是**可配置**的（配置文件内声明正则与字段名），而不是硬编码在二进制里。否则每一个改过日志格式的用户都会变成一个"解析不了"的 bug 报告。

---

## 5. 事件系统与命令树

### 5.1 事件系统

事件系统是日志能力的对外接口：解析器产出结构化事件，事件总线把它们分发给插件。

技术选型为 `tokio::sync::broadcast`（多生产者 / 多消费者，发送方不阻塞）。这个选择很合适，但它的语义必须被正确理解，否则会在生产环境里表现为"日志莫名其妙少了一段"：

| 语义 | 说明 |
| --- | --- |
| 有界容量 | 通道满时**淘汰最旧的消息**，而不是阻塞发送方 |
| `send()` 不等待 | 因此发送方永远不会被慢消费者拖住——这是它的优点，也是问题来源 |
| `RecvError::Lagged(n)` | 消费者落后时，**旧消息被静默丢弃**，只能通过该错误得知"丢了 n 条" |
| 消息克隆后才释放 | 消息在所有活跃接收方都取走前不会从通道中移除 |

> **建议（实现要点）**：在 `broadcast` 之上包一层 `EventBus`，把 `Lagged` 从"静默跳过"升级为**显式信号**——消费者收到 `Lagged` 后触发一次全量重读/重同步，而不是假装无事发生。
>
> **建议（分级）**：事件应按"可容忍丢失"与"不可丢失"分级。遥测类事件（心跳、计数、状态快照）走 `broadcast` 即可；关键业务事件（命令执行结果、权限变更）若要求不丢，应走有界 `mpsc` 配合落盘或持久化队列，而非 `broadcast`。

### 5.2 命令树

命令树为插件提供标准命令系统，同时衔接两类既有命令来源：

```
        ┌──────────────────────────────────────┐
        │              Atlas 命令树              │
        └───────────────┬──────────────────────┘
                        │
    ┌───────────────────┼───────────────────┐
    │                   │                   │
┌───▼────┐        ┌─────▼──────┐     ┌──────▼─────────┐
│ 插件命令 │        │ 服务端命令  │     │ 游戏内内置命令   │
│ (运行时注册)│       │ (终端/stdin) │     │ (/tp /gamemode) │
└────────┘        └────────────┘     └────────────────┘
```

三条来源的语义差别很大，路由时必须区分：

- **插件命令**：由插件在运行时注册，权限由 **Atlas 权限系统**判定。
- **服务端命令**：投递给某个服务端的控制台（stdin），执行主体是**该服务端本身**。
- **游戏内内置命令**：由**玩家**在其所在服务端执行，权限由**玩家在该服务端的游戏内权限等级**决定。

> **建议（路由与冲突）**：`clap` 的 `Command` 在运行时是可变的结构（`Command::new(..).subcommand(..)`），因此命令树可以用 clap 的 **builder API** 构建，但 derive 宏不适用于运行时动态注册。
>
> 由于存在三个命令来源，必须定义**命名空间与优先级**。建议：插件命令统一加前缀（`atlas:` 或插件自定义前缀），Atlas 内置命令保留裸名，游戏内内置命令由**服务端**而非 Atlas 仲裁，避免 Atlas 劫持 `/tp` 这类玩家已经习惯的指令。
>
> **建议（冲突检测）**：插件注册命令时应在注册阶段做重名检测并给出明确错误，而不是等到运行时"谁先注册谁赢"。

### 5.3 终端命令执行与结果采集

Atlas 可以在**玩家当前所在服务器**的终端执行命令，并采集返回信息。

技术上的实现要点是**结果边界**：Minecraft 服务端没有结构化的命令返回通道，命令输出最终仍以控制台行的形式回到 stdout。因此"采集返回信息"在实现上必然是**基于日志行的启发式**——执行命令后，向该服务端的输出流中等待若干行、把匹配到的内容作为结果。

> **建议（结果边界）**：由于返回信息依赖日志行的启发式匹配，结果必须携带"这是启发式提取的"这一属性，并明确**超时语义**（例如发出命令后 N 毫秒内没有匹配到输出即判定为空结果）。命令执行与结果采集应封装为"提交 → 等待 → 超时"的完整接口，而不是让插件自己拼装。

这条能力同时是 `!!op` 等特权动作的落地手段（见 §7）。

---

## 6. 插件系统

### 6.1 插件形态

Atlas 支持三种插件交付形态：

| 形态 | 构成 | 典型用途 |
| --- | --- | --- |
| 单文件插件 | 单个可执行/动态库文件 + 清单 | 简单工具类插件 |
| 打包插件 | `.atlas` 归档包（内含清单与产物） | 常规分发 |
| 源码插件 | `.rs` 源文件 | 现场修改、快速迭代 |
| 文件夹插件 | 目录结构（清单 + 产物 + 资源） | 需要附带资源文件的复杂插件 |

> **待确认**：`.atlas` 归档包与文件夹插件的**内部目录规范**（清单文件名、清单字段、资源目录约定）尚未在项目背景中定义。

> **重要取舍（建议先看）**：`.rs` 形态意味着 Atlas 在**运行时需要调用 Rust 编译器**。这带来三个现实约束：
>
> 1. 运行环境必须安装与 Atlas 匹配的 Rust 工具链；
> 2. 编译耗时是秒级到分钟级，**不适合"热重载"的实时场景**——它更像开发态便利，而非生产态热更新手段；
> 3. 依赖获取需要访问 crates.io（或本地 vendor 目录），在离线/受限网络中不可用。
>
> 建议把 `.rs` 明确定位为**开发态**插件形态，生产态以预编译产物为准；并提供依赖 vendoring 方案以支持离线编译。

### 6.2 插件可用能力

插件通过 Atlas 暴露的接口扩展业务能力，可用的接口面包括：

- 订阅事件流（解析后的日志事件、命令执行事件、服务器状态变化等）；
- 注册命令（挂载到 Atlas 命令树）；
- 调用终端命令执行接口（在指定服务端执行命令并采集返回信息）；
- 通过 SFTP 接口访问所管理服务器的文件；
- 管理自身生命周期（启用 / 禁用 / 卸载）。

### 6.3 加载与生命周期

插件的完整生命周期为：发现 → 加载 → 启用 → 运行 → （热重载 | 禁用）→ 卸载。

插件被禁用或重载时，Atlas 应当完成：停止该插件的事件订阅、注销其注册的命令、释放其持有的文件句柄与网络连接，然后再切换到新版本。

### 6.4 热重载的两条路径与取舍 ⚠️

项目背景中给出了两条热重载路径，二者**不是等价选项**，存在实质性的工程取舍：

#### 路径一：`cdylib` + `libloading`（动态库加载，Atlas 进程不重启）

**做法**：插件编译为 `cdylib`，Atlas 在运行时用 `libloading` 加载符号、调用入口函数；重载时卸载旧库、加载新库。

**风险（如实说明）**：

- **Rust 没有稳定的 ABI 承诺**。`dyn Trait` 的 vtable 布局、类型元数据编码等，在编译器版本之间都可能变化。把 `dyn Trait` 跨动态库边界传递，属于未定义行为。
- **即使编译器版本与编译参数完全一致，也不保证 ABI 兼容**。布局可能受随机化开关（如 `-Zrandomize-layout`）或 usage-driven 优化影响，Rust 对此不提供稳定性保证。
- **卸载 Rust 动态库本身有风险**。库内的静态状态、线程局部变量、panic 运行时、分配器假设都可能在 `dlclose` 后留下悬垂引用。
- **panic 跨界即 UB**。插件中的 panic 若跨越 FFI 边界展开，会直接破坏宿主进程。必须在边界处做 `catch_unwind`，或要求插件以 `panic = "abort"` 编译。
- **版本配套是运维负担**。插件必须用与 Atlas **完全相同**的 toolchain 与 feature 集合编译，升级 Atlas 即意味着所有插件需要同步重编。生态化的第三方插件几乎不可能满足这一约束。

> **结论**：该路径在"自己编译、自己部署"的场景下可以工作，但在**对外分发第三方插件**的场景下，ABI 风险是结构性的，不是可以靠小心规避的。真正向前兼容的做法是**稳定 C ABI**（`#[repr(C)]` 的纯 C 函数指针结构），Rust 生态中 `stabby` 一类 crate 正是把 C ABI 包装成接近 Rust 的写法。

#### 路径二：Workspace 重编译模型（重编译并重启 Atlas 进程）

**做法**：插件与 Atlas 位于同一 Cargo workspace；热重载时重新编译 workspace，重启 Atlas 进程，重新加载插件。

**这一路径有一个常被忽略的关键优势**：由于 **Minecraft 服务端是 Atlas 的独立子进程**，重启 Atlas 进程**不会关闭任何游戏服务器**。因此"重启宿主"在语义上依然满足"不关闭服务器"的产品承诺——游戏服持续在线，只有 Atlas 自身短暂重启。

代价：

- 重载期间 Atlas 短暂不可用（TUI 断开、事件流中断、远程 TUI 会话断开）；
- 每次重载需要完整编译，耗时明显长于动态库热替换；
- 插件需要以源码形式存在于 workspace 中（与 `.rs` / 文件夹插件形态天然契合）。

#### 建议的取舍

> **建议**：以**路径二为主线**——它同时规避了 ABI 风险、panic 跨界风险和第三方插件的 toolchain 依赖问题，代价（编译耗时与短暂中断）是可以接受的运维成本。
>
> **路径一保留为可选的"实验性/开发态"加速器**：明确标注其不保证 ABI 兼容，要求插件与宿主使用同一 toolchain 编译，并在边界处强制 `catch_unwind`。
>
> 若未来要支持**第三方插件市场**，则必须收敛到稳定 C ABI，或者改用**进程隔离**（插件作为独立进程运行，通过 IPC 与 Atlas 通信）。进程隔离的收益是：一个插件崩溃不会带走 Atlas，且热切换粒度更细；代价是引入序列化协议、跨进程通信与调试复杂度。

---

## 7. 权限模型

### 7.1 为什么权限系统是刚需

Atlas 的多条能力入口最终都会触达"以高权限执行某事"：

- 在服务端控制台执行命令（等价于该服务端的 OP 权限）；
- 在游戏内触发特权动作；
- 触发备份等运维操作；
- 通过远程 TUI 或插件发起上述操作。

因此**权限检查点不能只放在一个地方**，而必须覆盖每一条"能产生副作用"的路径。

### 7.2 `!!op` 例子所揭示的机制

项目背景给出的例子是：玩家在创造服务器中输入 `!!op`，即可获得该服务器的 OP 权限。

把这一例子的实现拆开，可以看出它必然涉及四步：

1. **采集**：Atlas 解析该服务端的聊天事件，识别出 `!!op` 这一消息；
2. **主体识别**：确定这条消息来自**哪个服务器、哪个玩家**，以及该玩家的 Atlas 身份与权限等级；
3. **授权**：用 Atlas 权限系统判定"该玩家是否有权请求此动作"；
4. **执行**：在**玩家所在的服务端终端**执行 `op <玩家名>`，并通过终端命令执行接口采集返回信息以确认是否成功。

这解释了为什么"终端执行命令并采集返回信息"是一项核心能力——它正是把游戏内意图转化为服务端特权操作、并确认结果的通用机制。

### 7.3 关键安全约束

> **权限判定绝不能基于消息文本。**
>
> 任何玩家都可以在聊天里打出 `!!op` 这样看起来像管理指令的文本；如果 Atlas 通过"消息长得像不像命令"来决定是否执行，攻击面就等于"任何人都能发任意文本"。同理，`op` 权限的授予、备份的触发，都必须由 Atlas 自己维护的**主体 → 权限 → 资源**三元组来判定，消息文本只作为参数，不作为凭证。
>
> **建议（授权模型）**：
>
> - **主体（Subject）**：游戏内玩家、远程 TUI 操作者、插件（插件应有独立的身份，且可被授予受限权限）；
> - **动作（Action）**：如 `server.execute`、`server.grant_op`、`server.backup`、`file.read`、`file.write`、`plugin.manage`；SFTP 侧可进一步细分为 `file.list`（浏览）、`file.download`、`file.upload`、`file.modify`、`file.delete`，以便只读运维账号与可写运维账号分离；
> - **资源（Resource）**：具体服务器、具体文件路径、具体插件。
>
> 判定粒度建议为「主体 × 动作 × 资源」，其中资源默认可细化到单个服务器，满足"玩家在创造服务器输入 `!!op`"这类**按服授权**的需求——即玩家对 A 服有授权，不代表对 B 服有授权。
>
> **建议（检查点清单）**：以下路径都应各自独立校验，不能只在一处兜底：
>
> - 多服务器开关操作（开服 / 关服 / 重启）；
> - SFTP 文件管理（浏览 / 上传 / 下载 / 修改 / 删除）；
> - 命令树执行（插件命令 / 服务器命令 / 游戏内内置命令，需按来源分别判定）；
> - TUI 子进程控制与主进程守护进程控制；
> - 触发备份等扩展管理操作。
>
> **建议（默认拒绝）**：未显式授予的组合一律拒绝。配置上应支持通配（如 `server.*`、`file.*`）以便运维省心，但**通配的授予必须与具体授予同等严格**——否则"给插件授予 `file.*`"就等于把全部服务器的文件写权限交出去了。
>
> **建议（审计）**：所有权限判定与特权执行都应落审计日志，记录主体、动作、资源、结果。这既是事后追责的依据，也是发现权限配置过宽的唯一手段。

---

## 8. Rust 技术选型

| 职责 | 选型 | 说明与注意事项 |
| --- | --- | --- |
| 异步运行时 | `tokio` | 多子进程 + 网络 + TUI 事件流的并发底座。优雅关闭建议配合 `tokio_util::sync::CancellationToken` 与 `TaskTracker` |
| 子进程管理 | `tokio::process` | 需并发读取 stdout 与 stderr；注意 `kill_on_drop` 与僵尸进程回收 |
| TUI 渲染 | `ratatui` + `crossterm` | ratatui 0.30.x 拆分为 `ratatui-core` 等子 crate；crossterm 需开启 `event-stream` feature 以在 async 环境中消费键盘事件 |
| 远程 / SFTP | `russh` + `russh-sftp` | SFTP v3 协议，支持 `limits@openssh.com`、`expand-path@openssh.com`、`fsync@openssh.com`、`statvfs@openssh.com`、`hardlink@openssh.com` 等扩展 |
| 命令树 | `clap` | 需用 builder API 而非 derive，以支持运行时动态注册 |
| 事件分发 | `tokio::sync::broadcast` | 语义见 §5.1，必须处理 `Lagged` |
| 插件动态加载 | `libloading` | ABI 风险见 §6.4 |

### 8.1 选型细节与已知约束

**`russh` / `russh-sftp`**

- `russh` 是 Tokio 之上的 SSH2 客户端 / 服务端实现，**必须启用至少一个加密后端 feature**（`aws-lc-rs` 或 `ring`），两个都不启用会编译失败。
- `russh-sftp` 与 `russh` **版本强耦合**，需成对升级；`russh-sftp` 3.x 对应 `russh` 0.63.x，二者都在持续做 breaking release。
- `russh-sftp` 官方提供了与 OpenSSH 的对比基准（见 `lib.rs/crates/russh-sftp`），但那是**库级别**数据，不等同于 Atlas 的端到端表现。
- 若"远程 TUI"最终也走 SSH，`russh` 的服务端 + PTY 通道可以与 SFTP 复用同一套凭据与连接管理，**这是本项目选型中复用度最高的一处**（建议）。

**`ratatui` + `crossterm`**

- ratatui 0.30 起采用模块化 workspace，widget 库应依赖 `ratatui-core` 而非完整 `ratatui`。
- **终端状态恢复是硬性要求**：Atlas 管理着子进程和 TUI，若异常退出时未恢复终端（raw mode、alternate screen、光标可见性），用户会得到一个不可用的终端。ratatui 提供的 `ratatui::run()` 会安装 panic hook 并在退出时恢复终端，**建议直接使用**。
- 建议开启 `crossterm` 的 `event-stream` feature，配合 `tokio::select!` 让键盘输入与后台事件在同一循环内处理。

**`clap` 与运行时命令树**

- derive 宏在编译期固化命令结构，无法用于运行时注册插件命令；命令树必须用 builder API 手工构建。
- 建议把"命令来源（Atlas 内置 / 插件 / 游戏内）"作为一等元数据挂在 `Command` 上，用于冲突检测、权限路由与 TUI 补全提示。

**`tokio::sync::broadcast`**

- 见 §5.1。最容易踩的坑是**把 `Lagged` 当作正常返回直接忽略**，表现为日志"偶尔少几行"且难以复现。

---

## 9. 当前范围与未来规划

### 9.1 已有能力 / 当前重点

- 子进程托管、多服务器批量开关服、TUI 与远程 TUI；
- SFTP 远程文件管理；
- **日志识别与解析（当前重点）**：Log4j2 家族与 CraftBukkit 家族双兼容，产出结构化事件；
- 事件系统、命令树、终端命令执行与结果采集；
- 插件系统：多形态插件 + 热重载 + 权限管理。

### 9.2 规划中：Mod 数据源

未来的数据来源规划有两条：

1. **Mod 获取接口**：通过 Mod 在游戏中采集信息，传输到 Atlas；
2. **Mod 接口返回信息**：Atlas 通过 Mod 接口向游戏侧取信息。

两种方式通过 **config 切换**，并对应不同的配置形态：

| 数据源 | 配置要点 |
| --- | --- |
| Mod 获取接口 | 需配置 **IP 等网络信息**（Mod 侧需知道往哪里连、连哪个端口） |
| 终端日志抓取 | 通过**相对路径或绝对路径**控制服务器控制台（相对 Atlas 工作目录解析，或直接给绝对路径） |

> **建议（协议设计）**：Mod 侧是 Java、Atlas 侧是 Rust，协议必须**独立于两种语言的实现**。建议使用「长度前缀 + JSON 或 CBOR」的 TCP 帧，并加入每台服务器的鉴权令牌与协议版本号——版本协商可以让"Mod 版本与 Atlas 版本不匹配"退化为一条可读的错误，而不是连接层的怪异行为。
>
> **建议（双数据源共存）**：由于是 config 切换而非运行时双开，日志抓取与 Mod 数据源在切换瞬间可能存在**信息空洞**。建议在切换时保留一段缓冲期的双写或明确标注数据源切换的时间点。
>
> **建议（相对/绝对路径）**：相对路径的解析基准必须明确且可配置（Atlas 进程工作目录 vs. 配置文件所在目录 vs. 每台服务器的根目录）。运维排查时"路径指到哪儿去了"是最常见的问题之一。

### 9.3 明确不做的事（建议）

保持边界清晰是 Atlas 长期可维护的前提。建议明确写入项目约定：

- 不实现 Minecraft 服务端本身的功能（经济、任务、领地、菜单等一律插件化）；
- 不接管游戏内已有命令的语义仲裁；
- 不在本体中内置任何具体业务策略。

---

## 10. 风险登记册

| # | 风险 | 影响 | 状态 / 缓解 |
| --- | --- | --- | --- |
| 1 | **插件热重载的 ABI 兼容性**：`cdylib` + `libloading` 跨编译器版本不保证 ABI 兼容，同版本同参数也不保证 | 加载即 UB、宿主崩溃、第三方插件不可用 | **未解决**。建议主线切换到 Workspace 重编译模型（§6.4）；`cdylib` 路径降级为开发态加速器并强制 `catch_unwind`；若要支持第三方分发，需收敛到稳定 C ABI 或进程隔离 |
| 2 | **动态库卸载的悬垂状态**：Rust 动态库卸载后残留静态状态 / 线程局部 / panic 运行时 | 延迟性崩溃，难以定位 | **未解决**。与风险 1 同源；进程隔离方案可根除 |
| 3 | **`broadcast` 的 `Lagged` 被静默忽略** | 日志 / 事件随机缺失，难以复现 | 建议在 `EventBus` 层升级为显式信号并触发重同步（§5.1） |
| 4 | **日志格式随部署方式与用户配置漂移** | 解析失效，事件字段缺失 | 建议 profile 可配置 + 解析失败降级不丢行 + 始终保留 `raw`（§4.2） |
| 5 | **权限判定依赖消息文本** | 任意玩家可伪装触发特权操作 | 建议主体/动作/资源三元组授权，文本只作参数不作凭证（§7.3） |
| 6 | **`.rs` 插件要求运行时 Rust 工具链** | 生产环境不可用、编译耗时长、离线不可编译 | 建议定位为开发态形态，生产走预编译产物 + 依赖 vendoring（§6.1） |
| 7 | **远程 TUI 的认证与加密方案未定** | 远程暴露控制面 | 待定方案；无论选型均需认证 + 审计 + 速率限制 |
| 8 | **`russh` / `russh-sftp` 版本强耦合且频繁 breaking** | 升级成本 | 建议在 `Cargo.toml` 中固定精确版本并建立升级验证流程 |
| 9 | **子进程管道读写并发不足造成死锁** | 服务端卡死、Atlas 无响应 | 建议 stdout / stderr 双任务并发读取 + 行缓冲策略（§3.1） |
| 10 | **终端状态未在异常退出时恢复** | 用户终端不可用 | 建议使用 `ratatui::run()`，并补充 panic / 信号处理路径的恢复 |

---

## 11. 术语表

| 术语 | 含义 |
| --- | --- |
| **Atlas 本体** | 精简的多服务器管理核心，直接负责子进程托管、TUI 控制、多服务器开关控制、SFTP 远程文件管理、日志解析、事件系统、命令树、插件加载与接口输出 |
| **Atlas Plugin** | 调用 Atlas 接口实现具体扩展业务功能的插件 |
| **子进程** | 由 Atlas 启动并托管的 Minecraft 服务端进程 |
| **守护进程** | 用于保障 Atlas 本体与受管子进程运行状态的主进程守护机制 |
| **事件系统** | 即日志功能获取接口，向插件提供日志事件订阅与消费能力 |
| **命令树** | 为插件提供标准命令系统，并衔接服务器命令系统与游戏内内置命令系统的统一命令机制 |
| **插件热重载** | 在不关闭服务器的情况下更新、启用或禁用插件 |
| **单文件 / 打包（`.atlas` / `.rs`）/ 文件夹插件** | Atlas 支持的三类插件分发与加载形态 |
| **终端命令返回信息接口** | 调用玩家当前所在服务器的终端执行命令，并采集返回信息所形成的接口 |
| **终端日志抓取** | 通过相对路径或绝对路径读取服务器控制台日志并解析 |
| **Mod 接口** | 通过 Mod 在游戏内获取信息、传输至 Atlas，或由 Mod 接口返回信息 |
| **远程 TUI** | 远程终端用户界面连接能力 |
| **SFTP** | SSH 文件传输协议（SSH File Transfer Protocol），用于远程管理服务器中的文件 |

---

## 12. 待确认信息

以下条目在项目背景中未明确，文中涉及处已标注"建议"或"待确认"。带 ✅ 的已在实现中定稿，其余仍需决策。

### 12.1 已定稿 ✅

| 事项 | 结论 |
| --- | --- |
| `.atlas` 归档包格式 | **tar 归档**，根目录含 `plugin.toml` 清单 + `entry` 指向的产物；损坏或缺件时跳过该插件并告警，不影响其它插件 |
| 文件夹插件规范 | 目录内含 `plugin.toml` 清单 |
| 单文件插件清单约定 | 优先级 1「同名 `<stem>.toml`」→ 优先级 2「插件根目录 `plugin.toml`」→ 都没有则不是插件 |
| 日志 profile 配置格式 | TOML `[[profile]]`，字段 `name` + `pattern`（正则须用 TOML 字面量字符串，反斜杠不转义）；同名 profile 原地替换内置项，新 profile 插到最前（优先级更高） |
| 插件热重载的主线路径 | **Workspace 重编译模型**（插件与 Atlas 同 workspace，重编译并重启 Atlas 进程）。因服务端是独立子进程，重启 Atlas 不关服 |
| 插件生命周期顺序 | 停止事件订阅 → 注销命令 → 释放句柄 → 卸载 → 加载新实例（已用 `PluginLifecycle` trait 建模并用 mock 验证调用序列） |
| 终端命令返回信息的采集语义 | 基于日志行的**启发式提取**，带超时窗口；窗口内无输出返回 `None`，有输出返回 `Some(文本)`。结果不保证完整或权威 |
| `ConsoleBridge` 的可测试性 | 抽取 `ConsoleSource` trait，`ManagedServer` 实现之，采集逻辑可用 mock 单测 |
| 首次运行的配置与目录 | `atlas init` 显式脚手架；任何子命令在配置缺失时也会自动生成 `atlas.toml` 模板并建好 `servers/`、`plugins/`、`keys/`、日志目录与各服务器 `workdir`。**已有配置不会被覆盖**（除非 `--force`） |
| SSH 客户端密钥的生成方式 | `atlas keygen` 显式命令，生成 OpenSSH 格式的 Ed25519 密钥对（私钥默认 `keys/id_ed25519`，Unix 权限 0600）。**不做静默自动生成**——私钥是凭据，且公钥无论如何要人工装到远端 `authorized_keys`。需要标准 PKCS#8 PEM 用 `--format pkcs8`（该格式支持 `--passphrase` 加密，PBKDF2-SHA256 迭代 600,000 次） |
| SFTP 客户端的连接语义 | Atlas 是 **SFTP 客户端**，`[sftp] host` 填**远端**服务器地址。未知主机默认拒绝（防中间人），首次连接用 `--accept-unknown-host` 或先写入 known_hosts |
| 客户端私钥的 ACL | `atlas keygen` 自动收紧：Unix `0600`，Windows `icacls` 收紧为「Administrators + SYSTEM」。不收紧的话 Windows 自带 `ssh`/`scp` 会以 "UNPROTECTED PRIVATE KEY FILE" 拒用该私钥 |
| Windows 服务端的公钥位置 | 管理员账户走 `C:\ProgramData\ssh\administrators_authorized_keys`（`sshd_config` 的 `Match Group administrators` 规则），且 ACL 须收紧到仅 Administrators + SYSTEM |

### 12.2 仍待决策

1. **远程 TUI 的传输层**：是否采用 SSH（`russh` 服务端 + PTY）？认证方式、主机密钥、审计粒度为何？
2. **插件跨边界的 ABI 形态**：宿主侧生命周期已建模，但**跨动态库边界的 ABI 仍未定稿**——Rust trait 边界（仅开发态）、稳定 C ABI（`#[repr(C)]`，对外分发）、还是进程隔离 + IPC？
3. **Mod 与 Atlas 之间的具体协议**：传输层、编码格式、鉴权、版本协商策略。
4. **"相对路径或绝对路径控制服务器控制台"的确切语义**：路径指向日志文件还是控制台输入；相对路径的解析基准。
5. **命令树的命名空间与优先级规则**：插件命令前缀、冲突检测与报错方式、游戏内内置命令的仲裁归属。
6. **SFTP 凭据管理**：多服务器的账号 / 密钥存储、密钥轮换、以及与远程 TUI 凭据是否统一。
7. **支持的服务端与部署形态范围**：是否需要兼容第三方面板代理的 stdout 形态；Cobble / Velocity 等代理端是否在范围内。
8. **`.rs` 源码插件的生产化路径**：运行时编译需要 Rust 工具链，依赖 vendoring 方案待定。
9. **是否需要贡献指南 / CI**：当前仓库缺少 CI 配置与 PR 模板。

