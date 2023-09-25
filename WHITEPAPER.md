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
    onion_packet   = pk(A) + enc(A, S, message2)
```

This packet is then sent like so:

```
sender --| onion_packet |--> A --| message2 |--> B --| message1 |--> C (actual_message)
```

Assuming A and B are trustworthy the sender is only revealed to `A` and receiver is only known to `B`.

The presented formula can be generalized over any number of intermediate nodes but minimum is 2 to preserve anonymity but more nodes can be used for amplified effect.

## Decentralized Message Buffer

Once we delivered our message to the node before recipient, in this case node `B`, we may extra flexibility by storing it in a distributed message buffer. This means node `C` does no need to be online all the time, rather it can query his messages later and decide to delete them sooner, to keep the amount of data in the network only scale by amount of users, the messages are only kept for limited amount of time and some upload limit should be incurred on users.

The storage is based on Distributed Hash Table (DHT) with predefined replication factor.

### Reputation (Idea)

Reputation is implemented trough off-chain workers. The worker can choose random node and query for message it knows should exist (it was anonymously sent it to fake address). The nodes that should have had it but does not have twice in a row, will loose the season reward.

