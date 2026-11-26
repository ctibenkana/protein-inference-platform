# syntax=docker/dockerfile:1
#
# ESM-2 embedding job. The model weights are baked in at a pinned revision,
# so jobs run fully offline and every image tag is reproducible.

ARG PYTHON_IMAGE=python:3.13-slim@sha256:7249d5587b78209a006dc4a3b5340678f84ea729239193aa559962cd8c1fce50

# --- build: dependencies + model weights ------------------------------------
FROM ${PYTHON_IMAGE} AS build

ARG TORCH_VERSION=2.15.0
ARG MODEL_ID=facebook/esm2_t6_8M_UR50D
ARG MODEL_REVISION=c731040fcd8d73dceaa04b0a8e6329b345b0f5df

ENV PIP_NO_CACHE_DIR=1 PIP_DISABLE_PIP_VERSION_CHECK=1
RUN python -m venv /opt/venv
ENV PATH=/opt/venv/bin:$PATH

# CPU-only torch: the GPU build would add several GB this job doesn't use.
RUN pip install torch==${TORCH_VERSION} --index-url https://download.pytorch.org/whl/cpu

COPY requirements.txt /src/requirements.txt
RUN pip install -r /src/requirements.txt

COPY pyproject.toml /src/
COPY src /src/src
RUN pip install --no-deps /src

RUN python - <<EOF
from huggingface_hub import snapshot_download
snapshot_download(
    "${MODEL_ID}",
    revision="${MODEL_REVISION}",
    local_dir="/opt/model",
    allow_patterns=["config.json", "model.safetensors", "*.txt", "*_config.json", "special_tokens_map.json"],
)
EOF

# --- runtime ----------------------------------------------------------------
FROM ${PYTHON_IMAGE}

ARG MODEL_ID=facebook/esm2_t6_8M_UR50D
ARG MODEL_REVISION=c731040fcd8d73dceaa04b0a8e6329b345b0f5df
LABEL org.opencontainers.image.title="esm-embed" \
      org.opencontainers.image.description="Batch ESM-2 protein embeddings" \
      org.opencontainers.image.licenses="Apache-2.0" \
      io.github.esm-embed.model="${MODEL_ID}@${MODEL_REVISION}"

# Fixed non-root UID so Kubernetes "restricted" Pod Security accepts it.
RUN useradd --uid 10001 --no-create-home --shell /usr/sbin/nologin app

COPY --from=build /opt/venv /opt/venv
COPY --from=build /opt/model /opt/model
COPY samples /opt/samples

ENV PATH=/opt/venv/bin:$PATH \
    PYTHONUNBUFFERED=1 \
    ESM_MODEL=/opt/model \
    HF_HUB_OFFLINE=1 \
    TRANSFORMERS_OFFLINE=1 \
    HF_HOME=/tmp/hf

USER 10001
EXPOSE 9100
ENTRYPOINT ["esm-embed"]
CMD ["--help"]
