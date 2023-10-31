# Orion Network

Decentralized network for anonymous data sharing interfaced trough messaging app.

## Overview


The network is composed of:
- source of truth and incentives (later just blockchain)
- onion routers and data distributors (later just miners)
- data producers and consumers (later just users)
- time interval starting with users committing tokens into network and ending with miners getting reward (later just season)

Blockchain:
- stores information about miners such as their public key and IP address, that is needed for nodes to organize
- allows the users to pay for the service, all of the payments are aggregated and at the end of the season split between miners as a reward
- incurs inflation on the network to incentivise transactions

Miners:
- offer onion routing service, user can fetch list of nodes from the blockchain and choose random onion route, the channel is then maintained for them
- temporarily store data (user messages) and make it available to recipients

Users:
- drive the economy by consuming the services offered by miners

## Onion Routing

To mask IP of users, their requests are anonymized trough onion routing protocol, following diagram describes the protocol bootstrap process:

```
    | blockchain |   | ephemeral key |  | message |
    +------------+   +---------------+  +---------+
          |                  |               |
          v                  |               |
    | miner list |           |               |
    +------------+           |               |
          |                  |               |
          v                  v               |
    | pick 3 miners | -> | onion packet | <--+
    +---------------+    +--------------+
```

This is done by the user, now lets look at the `onion packet` structure, for this example 3 miners involved in onion routing will be called `[A, B, C]`, `pk(N)` stands for public key of miner `N`, `enc(N, S, M)` stands for message `M` encrypted for miner `N` from sender `S`, `+` concatenates byte sequences:

```
    actual_message = "seret message for C"
    message1       = pk(C) + enc(C, S, actual_message)
    message2       = pk(B) + enc(B, S, message1)
    onion_packet   = pk(S) + enc(A, S, message2)
```

Each node then strips the sender public key, decrypts the message and replaces the next nodes public key with senders public key. This operation is denoted by `u(N, P)` where `N` is the miner performing the operation and `P` is the delivered packet.

This packet is then sent like so:

```
sender --| onion_packet |--> A --| u(A, onion_packet) |--> B --| u(B, u(A, onion_packet)) |--> C u(C, u(B, u(A, onion_packet))) = actual_message
```

Assuming A and B are trustworthy the sender is only revealed to `A` and receiver is only known to `B`.

The presented formula can be generalized over any number of intermediate nodes but minimum is 2 to preserve anonymity but more nodes can be used for amplified effect.

## Decentralized Message Buffer

The message buffer is implemented trough DHT (Kademila). Steps to send a message are as follows:

```rs
fn send_message(chat: Bytes, content: Bytes) {
    // this is not including onion routing for simplicity
    let replicating_nodes = DHT.get_closest_peers_to(chat);
    let peer = pick_random_from(repkication_nodes);
    let channel = connect_to(peer);
    channel.send(Request::SendMessage(chat, content));
}
```

Since we have the channel, we can subscribe to the chat so node will forward it when message gets replicated.

The encryption of messages is shifted to clients. We still need some amount of access control (sending messages). Each chat also contains public keys and associated permission level. First user to send a message is assigned permission 0, subsequent users invited can be assigned `inviter.perm..`, root users (perm == 0) can choose at which level the user can do certain things...
