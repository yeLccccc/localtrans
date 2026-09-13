# 中继服务器部署手册(阿里云 Ubuntu 实战版)

> 本手册按一次真实部署整理(v0.8.2),从「一台空服务器」到「手机在 4G 下连回家里的电脑」全流程。
> 中继非常轻量:二进制约 2MB、内存几十 MB、不落盘存储任何文件(只转发加密字节流),1 核 1G 的最低配机型即可跑。

## 0. 前置检查:磁盘还有余量吗

老服务器先看一眼,磁盘 100% 满时 systemd 起不来任何服务:

```bash
df -h /
```

如果根分区快满,常见病根是 **Docker 容器日志无限增长**(json 日志没配轮转,能涨到几十 G):

```bash
# 诊断:容器日志谁最肥
du -h /var/lib/docker/containers/*/*-json.log | sort -rh | head

# 清零回收(安全,不断容器;不要用 rm,运行中容器握着句柄空间不真释放)
truncate -s 0 /var/lib/docker/containers/*/*-json.log

# 除病根:配日志轮转上限,重启 docker 生效(容器会闪断重启)
cat > /etc/docker/daemon.json <<'EOF'
{
  "log-driver": "json-file",
  "log-opts": { "max-size": "50m", "max-file": "3" }
}
EOF
systemctl restart docker
```

> journal 日志过大同理:`journalctl --vacuum-size=100M`。

## 1. 拿到发布包

两种方式任选:

**方式 A:直接用发布包里的**(推荐)——`localtrans-relay-vX.Y.Z-ubuntu-x86_64.tar.gz` 在 PC 发布包(dist)里,内含:

```
localtrans-relay           # 二进制(静态链接,无运行时 so 依赖)
localtrans-relay.service   # systemd 单元
localtrans-relay.toml      # 配置模板
relay-deploy.md            # 本文档
```

**方式 B:服务器上从源码编译**(需要 Ubuntu 22.04+):

```bash
sudo apt update && sudo apt install -y build-essential pkg-config
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# 项目传上去后:
cd localTrans && cargo build --release -p localtrans-relay
```

## 2. 上传并安装

Windows 本机 PowerShell 上传(路径按你的习惯,下例放 `/root/work`):

```powershell
ssh root@服务器IP "mkdir -p /root/work"
scp localtrans-relay-vX.Y.Z-ubuntu-x86_64.tar.gz root@服务器IP:/root/work/
```

服务器上解压安装:

```bash
cd /root/work
tar xzf localtrans-relay-vX.Y.Z-ubuntu-x86_64.tar.gz
cp localtrans-relay /usr/local/bin/
cp localtrans-relay.service /etc/systemd/system/
cp localtrans-relay.toml /etc/localtrans-relay.toml
```

## 3. 配置

两样东西要填:**强随机 PSK**(客户端要填同一个)和**服务器公网 IP**:

```bash
# 生成 PSK(64 位 hex,先复制存好,别弄丢)
openssl rand -hex 32

# 查公网 IP
curl -s ifconfig.me; echo

# 编辑配置
nano /etc/localtrans-relay.toml
```

只改两行,其余默认即可:

```toml
public_ip = "查到的公网IP"     # 名册/会话地址拼接用;只支持 IP,不支持域名
psk = "生成的那串"             # v0.8.2 起短于 16 字符直接拒绝启动
```

配置项全表:

| 项 | 默认 | 说明 |
|---|---|---|
| `control_port` | 9443 | 控制面 QUIC(udp),客户端连这个 |
| `data_port_start/end` | 9000/9100 | 数据面端口池(udp),传输走这里 |
| `public_ip` | 必填 | 对外公布的公网 IP(拼进会话地址下发给客户端) |
| `psk` | 必填 | 预共享密钥,≥16 字符,建议 `openssl rand -hex 32` |
| `lease_ttl_secs` | 45 | 租约 TTL,一般不动 |
| `auth_max_per_min` | 5 | 认证失败速率限制(次/分),一般不动 |

## 4. 启动

```bash
systemctl daemon-reload
systemctl enable --now localtrans-relay
journalctl -u localtrans-relay -n 20 --no-pager
```

看到这两行即成功:

```
中继启动: 控制面 :9443 (udp), 数据面 9000-9100 (udp), 公网IP x.x.x.x
psk 摘要: xxxxxxxx (sha256 前 8 位, 供核对)
```

> **psk 摘要**是核对配置的神器:客户端连不上时,对比服务端日志的摘要和配置是否对应(改过配置忘重启是常见原因)。摘要不暴露密钥本身。
>
> 起不来会直接打「拒绝启动」+ 原因(v0.8.2 防线:配置缺失/解析失败/PSK 过短一律拒启,不会静默回退默认密钥)。

顺手验证端口在监听(应输出 ≥ 2,9443+9000~9100 共 101 个):

```bash
ss -uln | grep -cE ':(9443|9[01][0-9]{2})\b'
```

## 5. 安全组(阿里云)

ECS 控制台 → 实例 → 安全组 → 入方向手动添加两条:

| 协议 | 端口范围 | 授权对象 | 备注 |
|---|---|---|---|
| 自定义 UDP | 9443/9443 | 0.0.0.0/0 | 控制面 |
| 自定义 UDP | 9000/9100 | 0.0.0.0/0 | 数据面 |

> 服务器本机一般不用动防火墙(阿里云默认不启 ufw,拦截在安全组层)。若你启用了 ufw,另需 `ufw allow 9443/udp && ufw allow 9000:9100/udp`。

## 6. 客户端接入

**每一台**要跨公网互传的设备(PC / 手机)都填:

- 设置页 → 中继(跨公网)→ 打开开关
- 服务器:`服务器IP:9443`
- 密钥:与服务端一致的同一个 PSK
- 保存 → 状态变「已连接」

## 7. 验证

- 客户端设置页:中继状态「已连接」
- 设备页:出现带「远程」徽标的对方设备
- 点连接走配对(同意门 + 6 位码),互传一个文件,速度面板有 RTT/速度即全通

## 8. 日常运维

| 操作 | 命令 |
|---|---|
| 看实时日志 | `journalctl -u localtrans-relay -f` |
| 重启 | `systemctl restart localtrans-relay` |
| 改配置后生效 | 改 `/etc/localtrans-relay.toml` → `systemctl restart localtrans-relay` |
| 开机自启 | 已由 `enable` 生效,重启服务器自动拉起 |
| 升级版本 | 新二进制覆盖 `/usr/local/bin/localtrans-relay` → `systemctl restart localtrans-relay`(配置不动) |

## 9. 常见问题

| 现象 | 排查 |
|---|---|
| 服务起不来,报「拒绝启动」 | 配置文件缺失/路径不对/TOML 语法错/PSK < 16 字符,按日志提示改 `/etc/localtrans-relay.toml` |
| 客户端一直「连接中」 | ①安全组 UDP 9443 放了吗 ②PSK 两边一致吗(对摘要) ③服务器 IP 填对了吗 |
| 能连上但传输失败/卡住 | 安全组 UDP 9000-9100(数据面)没放 |
| 换了 PSK 后客户端连不上 | 改配置要 `systemctl restart` 才生效;客户端也要同步改 |
| 服务器重启后中继没起来 | `systemctl status localtrans-relay` 看状态,一般 `enable` 过会自动起 |
| 传输慢 | 受服务器带宽限制(阿里云按量计费注意出流量);中继只转发密文,与压缩无关 |
| 想看谁连着 | `journalctl -u localtrans-relay --since "5 min ago"`(日志只有指纹,无文件名) |

## 安全说明

- 全程端到端加密(QUIC + TLS 1.3):中继只见加密字节流,**看不到文件名、内容、传输双方的身份明文**
- PSK 只用于控制面准入认证;每对设备的会话密钥独立协商,不经过服务器
- 中继不落盘任何传输数据,重启即忘

## 10. 日志保留策略

数据面高频事件(NAT 漂移、KNOCK 回收等)在 v0.11.0 起已降为 debug 级,
默认 info 级日志量很小。但仍建议配置 journald 持久化 + 大小上限,防长跑积压:

```bash
# /etc/systemd/journald.conf 追加(或改):
#   Storage=persistent
#   SystemMaxUse=200M
sudo mkdir -p /var/log/journal
sudo systemd-tmpfiles --create --prefix /var/log/journal
sudo systemctl restart systemd-journald
```

- 持久化后重启不丢日志,便于排查中继重启前的问题
- `SystemMaxUse=200M` 限制磁盘占用,超出自动滚动删除最旧日志
- 查历史:`journalctl -u localtrans-relay --since "24 hours ago"`

本节随下次发布打包进 relay 包内同名文档。
