ARG BASE_IMAGE=ubuntu:24.04
FROM ${BASE_IMAGE} AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates libssl3t64 libstdc++6 libudev1 python3 \
    && rm -rf /var/lib/apt/lists/*
ENV PYTHONDONTWRITEBYTECODE=1

FROM runtime AS validator
COPY --chmod=0555 artifacts/validator/agave-validator /usr/local/bin/agave-validator
COPY healthcheck.py /opt/northstar/healthcheck.py
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/agave-validator"]

FROM runtime AS signer
COPY --chmod=0555 artifacts/signer/vrf-request-signer /usr/local/bin/vrf-request-signer
COPY healthcheck.py /opt/northstar/healthcheck.py
USER 10002:10002
ENTRYPOINT ["/usr/local/bin/vrf-request-signer"]

FROM runtime AS prover
COPY artifacts/prover/ /opt/northstar/prover/
COPY --chmod=0555 artifacts/prover/bin/northstar-zkvm-replay-script /opt/northstar/prover/bin/northstar-zkvm-replay-script
ENV LD_LIBRARY_PATH=/opt/northstar/prover/lib:/usr/local/nvidia/lib64 \
    ICICLE_BACKEND_INSTALL_DIR=/opt/northstar/prover/lib/backend
USER 10003:10003
ENTRYPOINT ["python3", "/opt/northstar/prover/gpu-worker.py"]
