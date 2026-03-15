from abc import ABC, abstractmethod


class ChannelPlugin(ABC):
    """Base class for all channel plugins"""

    name: str  # subclass must set as class variable

    @abstractmethod
    async def connect(self) -> None: ...

    @abstractmethod
    async def send_message(self, chat_id: str, text: str) -> None: ...

    @abstractmethod
    async def disconnect(self) -> None: ...

    def on_message_received(self, chat_id: str, sender: str, text: str) -> None:
        """Called when a message is received, routes to Rust Bus"""
        # TODO: Phase 3 - call _native.send_to_bus()
        pass
