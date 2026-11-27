# Spike S-B: one-port nginx, Buildbarn silo key, remote repo contents cache

Date: 2026-11-26
Expires: 2027-02-26
Triggered by: hex-architect high, rules_ocx under remote execution
Domain: remote execution

## Question

(a) Can one cleartext nginx port serve REAPI gRPC and registry HTTP/1.1, and does its JSON log witness registry bytes? (b) Does Buildbarn's reference config admit an arbitrary extra execution property, and does changing it miss the cache? (c) With a repository rule that runs `ocx package pull`, does `--experimental_remote_repo_contents_cache` (9.3.x) spare a fresh client the rule run, the registry and the repo-file download? (d) What is the cold-fetch baseline for one tool as a repository rule?

## What was run

Host: Linux 6.8, docker 29.10.0, compose v5.8.0. Compose project `arch-rbe-b`, only nginx published: `127.0.0.1:18400` (one port) and `127.0.0.1:18401` (gRPC-only fallback). Executor runner has `network_mode: none`.

Bazel: 9.3.3 (release binary, linux-x86_64) for the 9.3 legs, 9.2.0 (the repo toolchain binary) for one leg. Every call: `--ignore_all_rc_files --output_user_root=<scratch>/ob/<n>` first, `--repository_cache=<scratch>/ob/<n>-repocache` per output base, remotes only on 127.0.0.1. ocx 0.6.8 (copy of host binary), `OCX_HOME` and `HOME` in scratch, `OCX_NO_CONFIG=1`, `OCX_INSECURE_REGISTRIES=127.0.0.1:18400`.

Images (digests are `RepoDigests` as pulled):

| Service | Image | Digest |
|---|---|---|
| nginx | `nginx:1.29-alpine` (nginx/1.29.8) | `sha256:5616878291a2eed594aee8db4dade5878cf7edcb475e59193904b198d9b830de` |
| zot | `ghcr.io/project-zot/zot-linux-amd64:v2.1.23` | `sha256:b68f727fe35c1eb45b2ae91519dfb0e33192d69b5104882c3e7809b7146c4a02` |
| bb-storage (frontend, 2 storage) | `ghcr.io/buildbarn/bb-storage:20261102T091443Z-ca3d8b4` | `sha256:fecab707a842ee1915f2eea06810a1eb2eacc876d498648c67ba32230717f212` |
| bb-scheduler | `ghcr.io/buildbarn/bb-scheduler:20261102T094740Z-c9ba98c` | `sha256:50789bdae47ee83905c059f88adbf22c9253139883723f76fbed27def8b0c566` |
| bb-worker (hardlinking) | `ghcr.io/buildbarn/bb-worker:20261102T094740Z-c9ba98c` | `sha256:ba07db89d5cd6ae750497a38037abe127426c771a55c8a3da48c4205b929a32a` |
| bb-runner-installer | `ghcr.io/buildbarn/bb-runner-installer:20261102T094740Z-c9ba98c` | `sha256:3242e966d6de8a8d514db34e9e741b08f242200f23ad6094295915906acc26d5` |
| runner | `ghcr.io/catthehacker/ubuntu:act-22.04@sha256:dd7654ffb01d5b7b54b23b9ce928a1f7f2d08c7b3d7e320b6574b55d7ccde78b` | (pinned by digest) |

Buildbarn config is the upstream `bb-deployments` `docker-compose/config/*` (master, the image tags above), unmodified except for the tested worker edit in (b), reverted. Dropped from the upstream compose: portal, postgres, FUSE worker/runner. Frontend `instanceNamePrefix` of the worker is `hardlinking`; worker platform is `OSFamily=linux` + `container-image=docker://<runner image>`; scheduler routes by `platformKeyExtractor: { action: {} }`.

Fixture: `ocx package copy -p linux/amd64+libc.glibc --to 127.0.0.1:18400 ocx.sh/nodejs/node:24.21.0` (platform manifest bytes unchanged). Workspace: `MODULE.bazel` with one `use_repo_rule`, `repo.bzl` below, two genrules running `node --version` / `node -p ...` with the repo as `tools`. Bazel flags per build: `--remote_executor=grpc://127.0.0.1:18400 --remote_cache=<same> --remote_instance_name=hardlinking[/<silo>] --remote_default_exec_properties=OSFamily=linux --remote_default_exec_properties=container-image=docker://<runner image>`.

### Compose file (seeds the real rig; paths relative to the compose dir)

```yaml
name: arch-rbe-b
services:
  nginx:
    image: nginx:1.29-alpine
    ports: ['127.0.0.1:18400:80', '127.0.0.1:18401:81']
    volumes: ['./nginx/nginx.conf:/etc/nginx/nginx.conf:ro', './logs:/logs']
    depends_on: [frontend, zot]
  zot:
    image: ghcr.io/project-zot/zot-linux-amd64:v2.1.23
    expose: [5000]
    volumes: ['./zot/config.json:/etc/zot/config.json:ro', './volumes/zot:/var/lib/zot']
  frontend:
    image: ghcr.io/buildbarn/bb-storage:20261102T091443Z-ca3d8b4
    command: [/config/frontend.jsonnet]
    expose: [8980, 9980]
    volumes: ['./config:/config']
  storage-0:
    image: ghcr.io/buildbarn/bb-storage:20261102T091443Z-ca3d8b4
    command: [/config/storage.jsonnet]
    expose: [8981, 9980]
    volumes: ['./config:/config', './volumes/storage-ac-0:/storage-ac', './volumes/storage-cs-0:/storage-cs', './volumes/storage-cms-0:/storage-cms', './volumes/storage-fsac-0:/storage-fsac']
  storage-1:
    image: ghcr.io/buildbarn/bb-storage:20261102T091443Z-ca3d8b4
    command: [/config/storage.jsonnet]
    expose: [8981, 9980]
    volumes: ['./config:/config', './volumes/storage-ac-1:/storage-ac', './volumes/storage-cs-1:/storage-cs', './volumes/storage-cms-1:/storage-cms', './volumes/storage-fsac-1:/storage-fsac']
  scheduler:
    image: ghcr.io/buildbarn/bb-scheduler:20261102T094740Z-c9ba98c
    command: [/config/scheduler.jsonnet]
    expose: [8982, 8983, 8984, 9980]
    volumes: ['./config:/config']
  runner-installer:
    image: ghcr.io/buildbarn/bb-runner-installer:20261102T094740Z-c9ba98c
    volumes: ['./volumes/bb:/bb']
  worker-hardlinking-ubuntu22-04:
    image: ghcr.io/buildbarn/bb-worker:20261102T094740Z-c9ba98c
    command: [/config/worker-hardlinking-ubuntu22-04.jsonnet]
    volumes: ['./config:/config', './volumes/worker-hardlinking-ubuntu22-04:/worker']
  runner-hardlinking-ubuntu22-04:
    image: ghcr.io/catthehacker/ubuntu:act-22.04@sha256:dd7654ffb01d5b7b54b23b9ce928a1f7f2d08c7b3d7e320b6574b55d7ccde78b
    command: [sh, -c, 'while ! test -f /bb/installed; do sleep 1; done; exec /bb/bb_runner /config/runner-ubuntu22-04.jsonnet']
    network_mode: none
    volumes: ['./config:/config', './volumes/bb:/bb', './volumes/worker-hardlinking-ubuntu22-04:/worker']
    depends_on: [runner-installer]
```

Run directories created before `up`: `volumes/bb` and `volumes/worker-hardlinking-ubuntu22-04/{build,cs,cs/persistent_state}` mode 0777, `.../cache` 0700, `volumes/storage-{ac,cs,cms,fsac}-{0,1}/persistent_state` mode 0777, `volumes/zot` 0777, `logs/`. The upstream `run.sh` does the same with `sudo`.

### nginx.conf

```nginx
worker_processes 1;
events { worker_connections 1024; }
http {
  log_format j escape=json '{"t":"$time_iso8601","proto":"$server_protocol","method":"$request_method","uri":"$request_uri","status":$status,"req_bytes":$request_length,"sent_bytes":$bytes_sent,"body_sent":$body_bytes_sent,"ctype":"$content_type","port":"$server_port"}';
  access_log /logs/access.json j;
  client_max_body_size 0;
  proxy_request_buffering off;
  proxy_buffering off;
  # one cleartext port, h2c (gRPC) and HTTP/1.1 together
  server {
    listen 80;
    http2 on;
    location /v2/ { proxy_http_version 1.1; proxy_pass http://zot:5000; }
    location / {
      grpc_pass grpc://frontend:8980;
      grpc_read_timeout 3600s; grpc_send_timeout 3600s;
      grpc_buffer_size 4m;
    }
  }
  # two-port fallback: gRPC only on :81
  server {
    listen 81;
    http2 on;
    location / { grpc_pass grpc://frontend:8980; grpc_read_timeout 3600s; grpc_send_timeout 3600s; }
  }
}
```

### zot config

```json
{"distSpecVersion":"1.1.0","storage":{"rootDirectory":"/var/lib/zot"},"http":{"address":"0.0.0.0","port":"5000"},"log":{"level":"info"}}
```

### Repository rule (`repo.bzl`, the prototype shape)

```python
def _impl(ctx):
    log = "<scratch>/run/repo-rule-runs.log"
    ctx.execute(["sh", "-c", "echo \"$(date +%s.%N) start " + ctx.name + "\" >> " + log])
    home = ctx.path("scratch-home")
    env = {"OCX_HOME": str(home), "HOME": str(home) + "/user-home", "OCX_NO_CONFIG": "1", "OCX_INSECURE_REGISTRIES": "127.0.0.1:18400"}
    ctx.execute(["mkdir", "-p", str(home) + "/user-home"])
    r = ctx.execute([ctx.attr.ocx, "--format", "json", "package", "pull", "-p", "linux/amd64+libc.glibc", ctx.attr.id], environment = env)
    if r.return_code != 0:
        fail("pull failed: " + r.stderr)
    root = json.decode(r.stdout)["paths"].values()[0]
    ctx.execute(["mv", root + "/content", "content"])
    ctx.delete(home)
    ctx.execute(["find", "content", "-type", "l", "-delete"])
    ctx.execute(["sh", "-c", "echo \"$(date +%s.%N) end " + ctx.name + "\" >> " + log])
    ctx.file("BUILD.bazel", 'package(default_visibility = ["//visibility:public"])\nfilegroup(name = "all", srcs = glob(["content/**"]))\nexports_files(["content/bin/node"])\n')
    return ctx.repo_metadata(reproducible = True)

ocx_pkg = repository_rule(_impl, attrs = {"id": attr.string(mandatory = True), "ocx": attr.string(default = "<scratch>/bin/ocx")})
```

## Results

Registry traffic below is nginx's `sent_bytes` for `/v2/*`; REAPI bytes are nginx's `sent_bytes` for `ByteStream/Read` (client downloads) and `req_bytes` for `ByteStream/Write` (client uploads; for a streaming call this is the bytes read by nginx up to log time, so treat it as an upper-order figure, not exact).

Registry cost of one `ocx package pull` of node 24.21.0 (every leg that ran the rule): 6 requests = 1 `GET /v2/`, 3 manifests (2112 B), 2 blobs (39,099,991 B); total 39,102,453 B sent. Same numbers in all 6 legs where the rule ran.

| Leg (Bazel 9.3.3 unless noted) | Repo rule runs | Registry requests / bytes | Client ByteStream Read (n / bytes) | Bazel summary | Elapsed |
|---|---|---|---|---|---|
| ON ob1 cold (cold remote cache, no repo-contents entry) | 1 (3.0 s inside) | 6 / 39.1 MB | 1 / 116 B | 1 remote | wall 26.1 s |
| ON ob2 fresh output base + empty repository cache, builds ver + ver2 | 0 | 0 / 0 | 13 / 344,156 B | 1 remote cache hit, 1 remote (ver2 executed remotely) | wall 16.1 s |
| ON ob3, ob4 (repeats, fresh bases) | 0 | 0 / 0 | 12-13 / 344,040-344,155 B | cache hits | wall 16.4 s, 17.8 s |
| OFF ob1 cold | 1 | 6 / 39.1 MB | 1 / 116 B | 1 remote | wall 10.8 s |
| OFF ob2 fresh | 1 | 6 / 39.1 MB | 2 / 236 B | 1 remote cache hit, 1 remote | wall 10.7 s |
| OFF ob3 fresh | 1 | 6 / 39.1 MB | 2 / 236 B | 2 cache hits | wall 19.2 s |
| ON vs OFF, back-to-back, Bazel's `Elapsed time`, 2 rounds | - | - | - | - | ON 16.1 s, 20.1 s; OFF 23.3 s, 25.2 s |
| 9.2.0 ON ob1 | 1 | 6 / 39.1 MB | 2 / 236 B | 2 remote | 22.2 s |
| 9.2.0 ON ob2 fresh | 0 | 0 / 0 | 13 / 344,159 B | 2 remote cache hits | 14.1 s |

- In the ON ob2 leg the output base's `external/+ocx_pkg+node_linux_amd64` held only `REPO.bazel` and the generated BUILD (4 KB); the 4,800 node files were never on the client's disk. The OFF legs and ON ob1 hold the full tree (ON ob1: a symlink into the repository cache `contents/`).
- Wall times are noisy: the host carries other sessions, and each leg includes a fresh Bazel server start and install-base extraction. Range over all legs 8.4-26 s. No wall-time claim beyond "ON was not slower" survives; the registry and download numbers are exact.
- ON ob1 uploaded about 128 MB over 24 `ByteStream/Write` calls (the repo contents cache entry) on top of the ordinary action inputs; OFF ob1 also wrote about 128 MB over 5 calls (the node binary is one blob), so the upload cost is not a difference worth reading.

(d) baseline, repo-rule form, one tool (node 24.21.0, linux/amd64+libc.glibc, 4,801 files, 3 symlinks deleted in the rule): rule run 3.0 s (start to end line in the run log, 3.05 s and 3.03 s in two samples); 6 registry requests, 39.1 MB; the very first cold build against an empty remote CAS took 37.7 s wall end to end (3,028 individual `ByteStream/Write` calls for the input files, about 146 MB uploaded). Same build with warm CAS: 10.8 s.

(a) Port check, nginx 1.29.8, `listen 80; http2 on;` with `grpc_pass` for `/` and `proxy_pass` for `/v2/`:

| Request | Result |
|---|---|
| Bazel 9.3.3 build, `--remote_executor=grpc://127.0.0.1:18400` | works: remote execution and cache hits (all legs above) |
| `curl --http1.1 http://127.0.0.1:18400/v2/` | 200 (log: `HTTP/1.1`, port 80, 350 B sent) |
| `curl --http2-prior-knowledge http://127.0.0.1:18400/v2/` | 200, `HTTP/2.0` |
| `curl --http1.1 http://127.0.0.1:18401/v2/` (gRPC-only server block) | 415 |
| Bazel via the second port `18401` | works (cache hit) |

nginx JSON access log: one line per request, per gRPC call too (`uri` is the gRPC method), with `sent_bytes`, `body_sent`, `req_bytes`, `proto`, `port`. For registry GETs `sent_bytes` matched the blob size (39,099,991 B).

(b) The reference config rejects an unknown property at schedule time, not by queueing. `--remote_default_exec_properties=cache-silo-key=t1`:

```
ERROR: ... Executing genrule //:ver failed: (Exit 34): UNAVAILABLE: No workers exist for instance name prefix "hardlinking" platform {"properties":[{"name":"OSFamily","value":"linux"},{"name":"cache-silo-key","value":"t1"},{"name":"container-image","value":"docker://ghcr.io/catthehacker/ubuntu:act-22.04@sha256:dd76..."}]}
```

Two ways to admit a silo key:

| Change | Result |
|---|---|
| Add `{ name: 'cache-silo-key', value: 't1' }` to the worker's `runners[].platform.properties`, restart the worker | `t1` accepted and executed remotely; `t2` rejected with the same error. One key value per worker platform: not arbitrary. |
| No config change: `--remote_instance_name=hardlinking/<key>` (the worker's `instanceNamePrefix` is a prefix) | `hardlinking/t1`: 1 remote (miss); `hardlinking/t1` again after `bazel clean`: 1 remote cache hit; `hardlinking/t2`: 1 remote (miss); `hardlinking`: 1 remote cache hit. Arbitrary keys work, and the action cache is split per key. |

## Not run and why

- The hang of 9.2.0 (a ByteStream read timing out while loading repo contents) was not provoked; no timeout was injected. The 9.2.0 legs completed.
- Second backend, Windows and macOS clients, other platforms' repositories, signed packages, a registry that needs a login: not part of this spike.
- `--experimental_remote_downloader` (Remote Asset): not set; the Buildbarn reference setup has no Remote Asset service.
- Repo contents cache with a package that keeps symlinks in the repo (the rule here deletes them first, as the prototype did): not tested; issue 31456 lists silent exclusion because of symlinks as unfinished.
- Bazel's own bytes-downloaded line: not printed by the default summary; the number comes from nginx's `ByteStream/Read` instead. The BEP `systemNetworkStats` is host-wide and was not used.
- A cache-hit test that does not touch the network namespace of the client: client was not in a network namespace here; only the executor was.

## Answers

(a) Yes. `listen 80; http2 on;` (nginx 1.25.1+ syntax; 1.29.8 tested) serves h2c gRPC and HTTP/1.1 on one cleartext port, routed by path, no second port needed. The fallback second port also works. The JSON log records registry requests with bytes (`sent_bytes`), and REAPI calls per method.

(b) No arbitrary property: the scheduler rejects with `No workers exist ... platform {...}` immediately (exit 34). A property is admitted only if a worker platform lists it with that exact value. The usable silo is the instance name suffix: `--remote_instance_name=hardlinking/<key>` needs no config change and gives a clean miss per key and a hit on repeat.

(c) With the flag on (9.3.3), a fresh output base with an empty repository cache did not run the repository rule, made 0 registry requests, did not put the repo files on disk, and read about 344 KB (12-13 ByteStream reads) from the remote; a new action over those files (ver2) executed remotely. With the flag off the same fresh client reran the rule and pulled 6 requests / 39.1 MB again. The flag is a startup option. 9.2.0 behaved the same and did not hang in the one run, but the hang trigger was not provoked.

(d) Baseline, repo-rule form, node 24.21.0: rule run 3.0 s, 6 registry requests, 39.1 MB, per client that has no repo contents cache; a cold build into an empty CAS 37.7 s wall, same with warm CAS 10.8 s.

## How each check is shown red and green

- (a) Green: Bazel cache hit via 18400, HTTP 200 for HTTP/1.1 and h2c on 18400. Red reachable: the same HTTP/1.1 request against the gRPC-only block on 18401 returns 415, so the status is not constant.
- (b) Red: scheduler error above for an unadmitted property. Green: the same key accepted after the worker edit, and instance-name keys miss, hit, miss, hit in the sequence recorded. The worker file was restored and diffed against the original.
- (c) Red: the OFF legs (rule run count 1, registry 6 requests, in the repo-rule log and nginx log). Green: the ON ob2 leg (run count 0, 0 `/v2/` lines in that leg's slice of the log). The run log is truncated before each leg and counted with `grep -c start`; the nginx slice is taken by line count before and after each build, so a leg cannot read another leg's lines.
- (d) Same witnesses as (c): the rule's own log lines (start/end timestamps) and nginx's `/v2/` lines; both were seen non-zero in the OFF legs.
