# The crypto protocols

## Fundamentals

### Core encryption primitives

```mermaid
flowchart TB
    subgraph key_encapsulation
        EKeypair
        EPublicKey
        Ciphertext
    end
    kyber-->key_encapsulation
    x25519-->key_encapsulation
    aesGcm-->key_encapsulation

    subgraph signature_verification
        SKeypair
        SPublicKey
        Signature
    end
    dilitium-->signature_verification
    ed25519-->signature_verification

    subgraph hashing
        Hash
    end
    blake3-->hashing

    Nonce-->Proof
    SPublicKey-->Proof
    Signature-->Proof
```

### Onion routing

#### Setup

```mermaid
flowchart TB
    subgraph browser
        Client
    end

    subgraph "orion network"
        Node1
        Node2
        Recipient
    end

    subgraph pfn1["packet encrypted for Node1"]
        na2["Node2 Address"]
        subgraph pfn2["packet encrypted for Node2"]
            na3["Recipient Adress"]
            subgraph pfr["packet encrypted for recipient"]
            end
        end
    end

    Client --> pfn1 --> Node1 --> pfn2 --> Node2 --> pfr --> Recipient
    Client --> Secret
    Recipient --> Secret
```

#### Communication

```mermaid
flowchart TB
    subgraph Client
        Message --> aesGcmEncrypt
        Secret --> aesGcmEncrypt
    end

    subgraph Recipient
        s2[Secret] --> aesGcmDecrypt --> m2[Message]
    end

    aesGcmEncrypt --> Node1 --> Node2 --> aesGcmDecrypt
```

## Chat implementation comparison

### Central Buffer

```mermaid
sequenceDiagram
    actor C as Client
    participant CN as Command node
    participant SN as Subscription node
    actor R as Recipient

    Note over C,CN: Onion route
    Note over SN,R: Onion route
    Note over CN,SN: Together form an replication group
    C ->>+ CN: SendMessage(Proof, Content)
    CN ->> SN: Replicate(Proof, Content)
    CN ->>- C: MessageSent | ErrorOccured
    SN ->> R: SendMessage(Proof, Content)
```

The servers and also the recipient validate the proof attached to a message when it occurs. Problem appears when user retroactively reads the messages from the common buffer. Buffer cannot store the signatures for each message permanently since dilithium signatures are almost 5kb large. Malicious node can advertise false chat history to new nodes.

#### Lazy redistribution + Consistency voting

Lets go trough scenario where problems may occur:

- node enters/leaves the network
    - client connects to a node, since it discovers it as one of N closest nodes to data he is interested in
    - node does not have knowledge of this data due to it either being new or being pushed into replication group due to other node leaving
    - node tells client the chat does not exist (unwanted outcome)

To prevent this, requested node must verify its not supposed to have the data requested by querying closest nodes to the requested key. If the node is in fact in the group, it requests values from other nodes, majority of consistent values will be replicated and served to the client. The majority vote can be optimized to relief network bandwidth. Nodes will be sent common sequence of bytes which they combine with the hash of the value to be replicated and return that back to the requesting node. From majority matching hashes is one chosen to fetch the key.

```mermaid
sequenceDiagram
    actor C as Client
    participant NN as New node
    participant ON1 as Old Node 1
    participant ON2 as Old Node 2
    participant ON3 as Old Node 3

    C ->>+ NN: FetchMessages(Chat, Cursor)
    break is not part of replication group
        NN ->> C: Response(NotFound)
    end
    alt does not have chat
        par
            NN ->>+ ON1: GetHash(Chat, CommonBytes)
            ON1 ->>- NN: Response(Hash1)
        and
            NN ->>+ ON2: GetHash(Chat, CommonBytes)
            ON2 ->>- NN: Response(Hash1)
        and
            NN ->>+ ON3: GetHash(Chat, CommonBytes)
            ON3 ->>- NN: Response(Hash2)
        end
        NN ->>+ ON2: GetKey(Chat)
        ON2 ->>- NN: Response(History)
    end
    NN ->>- C: Response(Messages, NewCursor)
```

## Message multiplexing

Previous solution assumes majority of nodes are legitimate which is traded for performance. This implementation needs only one node in replication group to be honest though cannot be scaled to as many users. Instead of servers maintaining the state of the chat, members of the chat are remembered by each user along with history of messages. When inviting a user, the cached secret key is send along with the chat metadata to invitees mailbox, other users also receive message about this to their mailboxes. When removing a member, each user will validate this for themself and remove the user from recipients -> not sending the messages for the excluded user is enough since every user has separate secret.

```mermaid
sequenceDiagram
    actor C as Client
    participant M1 as Mailbox 1
    participant M2 as Mailbox 2

    Note over C,M2: Sending invites
    C ->> M1: EncapsulatedSecret1 + ChatMeta
    C ->> M2: EncapsulatedSecret2 + ChatMeta
    C ->> M1: Invited(Owner(Mailbox2))
    Note over C,M2: Sending messge
    C ->> M1: Encrypted(Message, Secret1)
    C ->> M2: Encrypted(Message, Secret2)
```
