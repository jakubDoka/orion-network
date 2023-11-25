# Onion Network Prototype

## This Prototype Demonstrates

Network is composed of decentralized servers that cooperate to replicate and serve client messages. They are split into swarms, groups of 5-10 servers and any number of clients listening for events (like messages). Clients is simply a holder of some unique identity that connects to a swarm, trough onion route, and interacts with the swarm trough requests (such as send message, read messages, create chat). Te messages are just sequences of arbitrary bytes associated with a chat. They are ordered by each server as they arrive, since consistent ordering of messages is not needed in practice. Clients will decide on format of their messages, recommended approach is to encrypt them with agreed upon symmetric key, of course more complex primitives such as Double Ratchet (Signal) can be used. The client in this prototype is browser based. Servers are tested on Linux, tho other platforms can be supported with little effort.

## This Prototype Does Not Demonstrate

Network does not offer any incentives to the servers, nor it confirms the server identity. No countermeasures against misbehaving servers are taken. The messages are nots stored permanently, instead the servers have message size cap and message count cap they are willing to store, when exceeded, oldest messages are deleted. All messages are stored in memory for simplicity, it's assumed the swarm redundancy is sufficient to preserve client data.

## Prior Art

This architecture is inspired by Session Messenger.
