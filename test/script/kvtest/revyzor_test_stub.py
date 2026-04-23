"""Revyzor KV Cache Connector - Test Stub for InferX integration.

Validates that the KVConnectorBase_V1 interface works correctly
in InferX's container environment. No compression logic -- just
interface verification and logging.

Replace with the production connector for actual compression.
"""

import os
import time
from dataclasses import dataclass, field

import torch

from vllm.distributed.kv_transfer.kv_connector.v1.base import (
    KVConnectorBase_V1,
    KVConnectorRole,
    KVConnectorMetadata,
)


@dataclass
class RevyzorConnectorMetadata(KVConnectorMetadata):
    requests_to_load: list = field(default_factory=list)
    requests_to_save: list = field(default_factory=list)


class RevyzorConnector(KVConnectorBase_V1):
    """Test stub for Revyzor KV cache compression.

    Logs KV cache operations without performing compression.
    Used to verify InferX container integration.
    """

    def __init__(self, vllm_config, role, kv_cache_config=None):
        super().__init__(vllm_config, role, kv_cache_config)
        self._role = role
        self._layer_count = 0
        self._query_count = 0
        self._total_bytes = 0
        self._pending_layers = []
        self._prefill_complete = False
        self._query_compressed = False
        print(f"[Revyzor-Test] Initialized as {role}", flush=True)

    def register_kv_caches(self, kv_caches):
        self._layer_count = len(kv_caches)
        print(f"[Revyzor-Test] Registered {self._layer_count} KV cache layers", flush=True)
        for name, tensor in list(kv_caches.items())[:2]:
            print(f"  {name}: shape={list(tensor.shape)} dtype={tensor.dtype}", flush=True)

    @classmethod
    def requires_piecewise_for_cudagraph(cls, extra_config=None):
        return True

    def save_kv_layer(self, layer_name, kv_layer, attn_metadata, **kwargs):
        if self._role != KVConnectorRole.WORKER:
            return

        slot_mapping = getattr(attn_metadata, 'slot_mapping', None)
        n_tokens = slot_mapping.numel() if slot_mapping is not None else 0

        if n_tokens <= 1:
            if not self._query_compressed and not self._prefill_complete:
                self._prefill_complete = True
            return

        if self._query_compressed:
            self._pending_layers = []
            self._prefill_complete = False
            self._query_compressed = False

        self._pending_layers.append({
            'layer_name': layer_name,
            'n_tokens': n_tokens,
            'bytes': kv_layer.numel() * kv_layer.element_size(),
        })

    def wait_for_save(self):
        if self._role != KVConnectorRole.WORKER:
            return

        if not self._pending_layers or not self._prefill_complete or self._query_compressed:
            return

        # Deduplicate
        unique = {}
        for info in self._pending_layers:
            unique[info['layer_name']] = info
        pending = list(unique.values())

        total_bytes = sum(p['bytes'] for p in pending)
        self._total_bytes += total_bytes
        self._query_count += 1

        print(f"[Revyzor-Test] Q#{self._query_count}: {len(pending)} layers, "
              f"{total_bytes/1e6:.1f}MB KV cache "
              f"(compression would happen here)",
              flush=True)

        self._pending_layers = []
        self._query_compressed = True

    def start_load_kv(self, forward_context, **kwargs):
        pass

    def wait_for_layer_load(self, layer_name):
        pass

    def get_num_new_matched_tokens(self, request, num_computed_tokens):
        return 0, False

    def update_state_after_alloc(self, request, blocks, num_external_tokens):
        pass

    def build_connector_meta(self, scheduler_output):
        return KVConnectorMetadata()

    def shutdown(self):
        if self._total_bytes > 0:
            print(f"[Revyzor-Test] Total: {self._query_count} queries, "
                  f"{self._total_bytes/1e9:.2f}GB KV cache processed",
                  flush=True)
