"""Bounded, process-local caches for reusable model work."""

from collections import OrderedDict

import numpy as np
import torch


class BudgetLRU:
    def __init__(self, entries, byte_budget):
        self.entries = entries
        self.byte_budget = byte_budget
        self.bytes = 0
        self.items = OrderedDict()

    def get(self, key):
        item = self.items.get(key)
        if item is None:
            return None
        self.items.move_to_end(key)
        return item[0]

    def discard(self, key):
        item = self.items.pop(key, None)
        if item is not None:
            self.bytes -= item[1]

    def put(self, key, value, size):
        self.discard(key)
        if not 0 <= size <= self.byte_budget:
            return
        self.items[key] = (value, size)
        self.bytes += size
        while self.bytes > self.byte_budget or len(self.items) > self.entries:
            _, (_, size) = self.items.popitem(last=False)
            self.bytes -= size


def state_bytes(state):
    """Count unique tensor storage retained after reset, including aliased views."""
    seen = set()
    storages = {}

    def count(value, depth=0):
        if id(value) in seen or depth > 8:
            return
        seen.add(id(value))
        if isinstance(value, torch.Tensor):
            storage = value.untyped_storage()
            storages[(str(value.device), storage.data_ptr())] = storage.nbytes()
        elif isinstance(value, np.ndarray):
            storages[("numpy", id(value))] = value.nbytes
        elif isinstance(value, dict):
            for child in value.values():
                count(child, depth + 1)
        elif isinstance(value, (list, tuple)):
            for child in value:
                count(child, depth + 1)

    for key in ["images", "cached_features", "constants"]:
        count(state.get(key))
    return sum(storages.values())
