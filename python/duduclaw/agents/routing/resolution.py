"""ResolutionPolicy engine framework for LazyRef resolution.

Architecture
------------
``Resolver`` is an abstract base class.  Concrete implementations (one per
``ref_type``) are registered with a :class:`ResolutionPolicy` instance.

When a :class:`~duduclaw.agents.routing.types.LazyRef` needs resolving, the
engine looks up the registered resolver by ``ref_type`` and delegates.

Design goals
------------
* **Plugin model**: Each engineering team implements their own ``Resolver``
  subclass (memory, wiki, artifact, …) without touching this module.
* **Safe failure**: Resolution errors are captured in :class:`ResolutionResult`
  rather than raising — callers decide whether missing refs are fatal.
* **Async-native**: All resolution is ``async`` to support I/O-bound backends.
* **Concurrent**: :meth:`ResolutionPolicy.resolve_all` resolves multiple refs
  concurrently via ``asyncio.gather``.

Usage
-----
::

    from duduclaw.agents.routing.resolution import ResolutionPolicy, Resolver
    from duduclaw.agents.routing.types import LazyRef

    class MemoryResolver(Resolver):
        @property
        def ref_type(self) -> str:
            return "memory"

        async def resolve(self, ref: LazyRef) -> str:
            return await memory_client.get(ref.ref_id)

    engine = ResolutionPolicy()
    engine.register(MemoryResolver())

    result = await engine.resolve(
        LazyRef(ref_type="memory", ref_id="key-abc", resolution_policy="lazy")
    )
    if result.resolved:
        print(result.value)
"""

from __future__ import annotations

import asyncio
from abc import ABC, abstractmethod
from dataclasses import dataclass
from typing import Any, Optional

from .types import LazyRef


# ── Errors ────────────────────────────────────────────────────────────────────


class ResolutionError(Exception):
    """Raised by a :class:`Resolver` when the referenced resource cannot be fetched.

    Catching this exception inside :meth:`ResolutionPolicy.resolve` converts it
    into a :class:`ResolutionResult` with ``resolved=False`` so that callers
    can handle missing refs gracefully.
    """


# ── Result model ──────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class ResolutionResult:
    """Outcome of a single :class:`LazyRef` resolution attempt.

    Attributes:
        ref:      The :class:`LazyRef` that was resolved.
        value:    The resolved resource value; ``None`` when ``resolved=False``.
        resolved: ``True`` if resolution succeeded, ``False`` otherwise.
        error:    Human-readable error message; ``None`` when ``resolved=True``.
    """

    ref: LazyRef
    value: Any
    resolved: bool
    error: Optional[str] = None


# ── Abstract resolver base ────────────────────────────────────────────────────


class Resolver(ABC):
    """Abstract base class for :class:`LazyRef` resolvers.

    Subclasses are registered with a :class:`ResolutionPolicy` and invoked
    whenever a :class:`LazyRef` whose ``ref_type`` matches
    :attr:`ref_type` needs to be fetched.

    Implementing a new resolver
    ---------------------------
    ::

        class WikiResolver(Resolver):
            @property
            def ref_type(self) -> str:
                return "wiki"

            async def resolve(self, ref: LazyRef) -> str:
                page = await wiki_client.read(ref.ref_id)
                return page.body
    """

    @property
    @abstractmethod
    def ref_type(self) -> str:
        """The ``ref_type`` string this resolver handles (e.g. ``"memory"``)."""
        ...

    @abstractmethod
    async def resolve(self, ref: LazyRef) -> Any:
        """Fetch and return the resource identified by *ref*.

        Args:
            ref: The :class:`LazyRef` to resolve.

        Returns:
            The resolved resource.  The exact type depends on the resolver.

        Raises:
            ResolutionError: When the resource cannot be fetched.
        """
        ...


# ── Resolution engine ─────────────────────────────────────────────────────────


class ResolutionPolicy:
    """Orchestrates :class:`LazyRef` resolution via registered :class:`Resolver` s.

    Resolvers are registered by ``ref_type``; only one resolver per type is
    kept (last registration wins).

    Usage::

        engine = ResolutionPolicy()
        engine.register(MemoryResolver(memory_client))
        engine.register(WikiResolver(wiki_client))

        result = await engine.resolve(ref)
        results = await engine.resolve_all(refs)
    """

    def __init__(self) -> None:
        self._resolvers: dict[str, Resolver] = {}

    # ── Registration ─────────────────────────────────────────────────────────

    def register(self, resolver: Resolver) -> None:
        """Register *resolver* for its declared :attr:`~Resolver.ref_type`.

        If a resolver for the same ``ref_type`` was already registered it is
        silently replaced (last-write wins).

        Args:
            resolver: A concrete :class:`Resolver` implementation.
        """
        self._resolvers[resolver.ref_type] = resolver

    def get_resolver(self, ref_type: str) -> Optional[Resolver]:
        """Return the registered resolver for *ref_type*, or ``None``."""
        return self._resolvers.get(ref_type)

    @property
    def registered_types(self) -> list[str]:
        """Return sorted list of registered ``ref_type`` strings."""
        return sorted(self._resolvers)

    # ── Resolution ────────────────────────────────────────────────────────────

    async def resolve(self, ref: LazyRef) -> ResolutionResult:
        """Resolve a single :class:`LazyRef`.

        If no resolver is registered for ``ref.ref_type``, returns a
        :class:`ResolutionResult` with ``resolved=False`` (no exception raised).

        Args:
            ref: The :class:`LazyRef` to resolve.

        Returns:
            A :class:`ResolutionResult` with the outcome.
        """
        resolver = self._resolvers.get(ref.ref_type)
        if resolver is None:
            return ResolutionResult(
                ref=ref,
                value=None,
                resolved=False,
                error=f"No resolver registered for ref_type='{ref.ref_type}'",
            )

        try:
            value = await resolver.resolve(ref)
            return ResolutionResult(ref=ref, value=value, resolved=True)
        except ResolutionError as exc:
            return ResolutionResult(
                ref=ref,
                value=None,
                resolved=False,
                error=str(exc),
            )
        except Exception as exc:  # noqa: BLE001
            return ResolutionResult(
                ref=ref,
                value=None,
                resolved=False,
                error=f"Unexpected error: {exc}",
            )

    async def resolve_all(self, refs: list[LazyRef]) -> list[ResolutionResult]:
        """Resolve multiple :class:`LazyRef` s concurrently.

        Failures in individual refs do **not** cancel others — every ref gets
        a :class:`ResolutionResult` regardless.

        Args:
            refs: List of :class:`LazyRef` s to resolve.

        Returns:
            List of :class:`ResolutionResult` in the same order as *refs*.
        """
        if not refs:
            return []
        return list(await asyncio.gather(*[self.resolve(ref) for ref in refs]))
