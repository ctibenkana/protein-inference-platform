"""ESM-2 embedder: one mean-pooled vector per protein."""

from __future__ import annotations

from typing import Sequence

import numpy as np


class Embedder:
    def __init__(self, model: str, threads: int | None = None) -> None:
        import torch
        from transformers import AutoTokenizer, EsmModel

        if threads:
            torch.set_num_threads(threads)
        self._torch = torch
        self.tokenizer = AutoTokenizer.from_pretrained(model)
        self.model = EsmModel.from_pretrained(model, add_pooling_layer=False).eval()
        self.dim: int = self.model.config.hidden_size
        # Positions include the <cls> and <eos> tokens.
        self.max_residues: int = self.model.config.max_position_embeddings - 2

    def embed(self, sequences: Sequence[str]) -> np.ndarray:
        """Return a float32 array of shape (len(sequences), dim).

        Each vector is the mean of the final-layer representations of the
        sequence's residues; start, end and padding tokens are excluded, so
        a protein's embedding doesn't depend on what it was batched with.
        """
        torch = self._torch
        enc = self.tokenizer(
            list(sequences),
            padding=True,
            return_tensors="pt",
            return_special_tokens_mask=True,
        )
        special = enc.pop("special_tokens_mask").bool()
        with torch.inference_mode():
            hidden = self.model(**enc).last_hidden_state
        keep = (enc["attention_mask"].bool() & ~special).unsqueeze(-1).to(hidden.dtype)
        pooled = (hidden * keep).sum(dim=1) / keep.sum(dim=1).clamp(min=1)
        return pooled.float().cpu().numpy()
