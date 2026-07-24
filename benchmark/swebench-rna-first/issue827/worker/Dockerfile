FROM ubuntu@sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982 AS builder
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends gcc libc6-dev && rm -rf /var/lib/apt/lists/*
COPY landlock_launcher.c /src/landlock-launcher.c
RUN gcc -O2 -Wall -Wextra -Werror -static -o /landlock-launcher /src/landlock-launcher.c && strip /landlock-launcher

FROM ubuntu@sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends python3 python3-pip strace bash ca-certificates && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /opt/rna827
COPY --from=builder /landlock-launcher /opt/rna827/landlock-launcher
COPY offline_worker.py /opt/rna827/offline-worker
RUN chmod 0555 /opt/rna827/landlock-launcher /opt/rna827/offline-worker
