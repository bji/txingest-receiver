
# Quick Start Instructions

**Build/Install Validator**

If using JITO, check out and build the txingest version of the JITO validator:

```$ git clone https://github.com/bji/solana.git```

```$ cd solana```

```$ git checkout v1.17.31-jito_txingest```

```$ git submodule update --init --recursive```

```$ ./cargo build --release```

If *not* using JITO, check out and build the txingest version of the normal validator:

```$ git clone https://github.com/bji/solana.git```

```$ cd solana```

```$ git checkout v1.17.31_txingest```

```$ ./cargo build --release```

Either way, the resulting solana-validator, from target/release/solana-validator, needs to be put
onto your validator in place of the existing solana-validator binary.

Also you need to ensure that an extra option is passed to the solana-validator binary.  If you do not
include this, nothing bad will happen; the validator will operate as normally, it just won't ever try
to connect to your txingest-receiver.

The option is: ```--txingest-host HOST:PORT```.  Although you can forward events to any host, it makes
the most sense to forward to a txingest-receiver process running on the validator itself, so assuming
you want to use port 15151 (any unused port will do), the option would be:

```--txingest-host 127.0.0.1:15151```.

**Build/Install JITO Relayer (optional)**

If using JITO, you must also be running your own JITO relayer, otherwise the events that indicate what
source ip addresses transactions are coming from will never be available to you.  You must check out
and build a slightly modified version of the JITO transaction relayer:

```$ git clone https://github.com/bji/jito-relayer.git```

```$ cd jito-relayer```

```$ git checkout v0.1.12_txingest```

```$ git submodule update --init --recursive```

Because the JITO relayer builds against the Solana SDK, and you must reference the *txingest* version
of the SDK when building it, you must modify the Cargo.toml file in the jito-relayer source and
update the "/path/to/your/solana" text to instead be a path to the Solana repo that you checked out
in the first steps above.  For example if you checked it out into "/home/me/solana", then you
would replace /path/to/your/solana with /home/me/solana.  One easy way to do this is with the following
command based on the following:

``` $ sed -i 's|/path/to/your|/home/me|g' Cargo.toml ```

Then build the JITO relayer:

```$ cargo build --release```

Then you have to install it onto your validator and run it instead of the stock JITO relayer.  Just like
with the validator, it is harmless to run these changes without having a txingest receiver running; and
just like for the validator, you do need to add a ```--txingest-host HOST:PORT``` argument, for example
```--txingest-host 127.0.0.1:15151```.

Then check out the txingest-receiver repo:

```$ git clone https://github.com/bji/txingest-receiver.git```

You have to modify the Cargo.toml in the txingest-receiver repo, just like you did or would
have done for the jito Cargo.toml: replace /path/to/your/solana with the local solana repo that you
checked out in the first steps above.  One easy way to do this is with the following
command based on the following:
``` $ sed -i 's|/path/to/your|/home/me|g' Cargo.toml ```

**Build/Install txingest-receiver**

Finally, build the txingest-receiver:

```$ cd txingest-receiver```

```$ cargo build --release```

And copy the binary onto your validator.  See the 'running' section below for instructions on how
to run it.  It's harmless to run it before or after your validator or relayer has started; all of
the validator, relayer, and txingest-receiver simply wait to make or receive connections when
they are available, and operate normally otherwise.

# txingest receiver

This implements a simple txingest receiver.  In order to function, it requires that a Solana validator
be patched with the txingest patch; see:

https://github.com/bji/solana/tree/v1.17.31_txingest

Or for JITO validators:

https://github.com/bji/solana/tree/v1.17.31-jito_txingest

This simple receiver will listen for connections the validator on a given port, and when connected
to, receive events from the validator, collate them, and log details about QUIC connections and
other validator events.

The receiver can be started and shut down at any time; the validator will automatically attempt
to connect/re-connect once per second, and will gracefully handle a terminated txingest connection.

If the validator had buffered events while waiting for a connection, it will clear out the oldest
events before submitting new events to the receiver, so that the receiver doesn't receive old
events that may have a "gap" in them due to how the validator's event buffers might fill while
waiting for a txingest connection.

# interaction with JITO

If a validator uses JITO, then it should run its own relayer, and should also modify the
JITO relayer to utilize txingest.  This will allow the JITO relayer to deliver events about
QUIC connections, since the JITO relayer is the QUIC endpoint that tx sources connect to.

The JITO relayer should be updated to use txingest.  The following branch contains the
change necessary to enable the JITO relayer to use txingest:

https://github.com/bji/jito-relayer/tree/v0.1.12_txingest

When JITO is used with txingest:

- The JITO relayer receives QUIC connections and sends events about the connections and tx
  delivered on them to the txingest receiver
- The validator receives the transactions from the relayer (either directly, or via JITO
  bundles) and sends txingest events describing the fee characteristics of the transactions
- Also the validator sends additional events like when it's leader slots are upcoming or
  are in progress

When a validator does NOT use JITO but does use txingest:

- The validator receives QUIC connections and sends events about the connections and tx
  delivered on them to the txingest receiver
- The validator sends txingest events describing the fee characteristics of the transactions
- Also the validator sends additional events like when it's leader slots are upcoming or
  are in progress

# running

Call with either two arguments:

```  txingest-receiver <LISTEN_ADDRESS> <PORT>```

Or three arguments:

```  txingest-receiver --show_tx <LISTEN_ADDRESS> <PORT>```

Typically this would be run on the validator itself, and would be run like so:

```  txingest-receiver 127.0.0.1 25555```

The above will listen only on 127.0.0.1 at port 25555 so no one from outside of the
system could receive events.  The same host and port must be passed to the validator
and JITO via their --txingest-host arguments.

If ```--show_tx``` is included, then lines describing every tx received on every QUIC
connection are logged.  This can be very voluminous, so probably only would be
used for limited debugging/analysis purposes.

# functionality

This txingest receiver is intended to collate information into events that would be used
to create firewall rules to block out peers who are creating QUIC connections with
certain undesirable characteristics.

Some examples of the kinds of rules that could be created and are under consideration by
the author of txingest:

- If an unstaked QUIC connection sends events more than 2 slots before or 2 slots after
  the validator's leader slots, then firewall that sender

- If an unstaked QUIC connection sends too many duplicate tx, then firewall that sender

- If a remote peer makes too many QUIC connections overall, then firewall that sender

- If a remote peer has too many failed QUIC handshakes, or exceeded its QUIC connection
  budget too often, then firewall that sender

- If an unstaked peer frequently makes QUIC connections that never deliver a transaction,
  then firewall that sender

- If a remote peer only submits large numbers of low fee paying tx, firewall that sender

# the log lines - common fields

Log line are generally of the form:

```<TIMESTAMP> <VERB> <PEER> <DETAILS>...```

```<TIMESTAMP>``` is always present and is the number of milliseconds since Unix epoch of the
event.

```<VERB>``` is always present and indicates what is being logged.

```<PEER>``` is usually present and identifies the remote peer by IP address, and for established
QUIC connections, also indicates the remote port.

```<DETAILS>``` are always present, but vary by event logged.  See below.

# the log lines - specifics

The following lines are logged:

```<TIMESTAMP> failed <REMOTE_ADDRESS> <COUNT>```
 - Logged every 100 connection establishment protocol failures or every 1 second for a
    given remote address, whichever comes first.  ```<COUNT>``` is the number of failures for
    this remote address that occurred within the previous 10 seconds.

```<TIMESTAMP> exceeded <REMOTE_ADDRESS> <PEER_PUBKEY> <STAKE> <COUNT>```
 - Logged every 100 connection establishment failures due to remote peer exceeding
    connection limits, or every 1 second for a given remote address, whichever comes
    first.  <PEER_PUBKEY> is either the pubkey of the peer, if known, or "none".
    ``<STAKE>``` is the lamports of stake of the remote peer.  NOTE that currently
    this is always 0; this field will likely be removed soon so don't count on it.
    ```<COUNT>``` is the number of failures due to exceeded connection limits for this
    remote address that occurred within the previous 10 seconds.

```<TIMESTAMP> started <REMOTE_ADDRESS:REMOTE_PORT> <STAKE>```
  - Logged on every new QUIC connection.  It is unclear exactly how the lifetime of
     QUIC connections are managed within the validator code base, so the timespan between
     a "started" and "closed" or "dropped" log *may* span multiple individual QUIC
     connections.  <PEER_PUBKEY> is either the pubkey of the peer, if known, or "none".
     ```<STAKE>``` is 0 for unstaked peers, or the lamports of stake
     for staked peers.

```<TIMESTAMP> inactive <REMOTE_ADDRESS:REMOTE_PORT> <STAKE>```
  - Logged when a QUIC connection has been established but has not delivered any
     events to the validator for 10 seconds.  It is logged one time only for
     each QUIC connection and only if no transactions are delivered on that
     connection for 10 seconds.  ```<STAKE>``` is 0 for unstaked peers, or the lamports of
     stake for staked peers.

```<TIMESTAMP> active/dropped/closed <REMOTE_ADDRESS:REMOTE_PORT> <STAKE> <DURATION> <NUM_DUP_TX> <NUM_VOTE_TX> <NUM_USER_TX> <NUM_BADFEE_TX> <NUM_FEE_TX> <MIN_FEE> <MAX_FEE> <TOTAL_FEES>```
  - active is logged every time there are changes but no more frequently than once
     per 10 seconds for a given connection.
  - dropped is logged 30 seconds after a QUIC connection has been closed by the
     validator itself.  Typically this is because the validator has closed the
     connection to make room for new connections.
  - closed is logged 30 seconds after a QUIC connection has been closed by the
     remote peer.
  - There are a lot of details provided so they are listed individually here,
     along with their column number, to assist in parsing out specific details of
     interest:

    Column 4: ```<STAKE>``` - 0 for unstaked peers, or the lamports of stake for staked
      peers.

    Column 5: ```<DURATION>``` - duration, in milliseconds, of the QUIC connection thus
      far.
  
    Column 6: ```<NUM_DUP_TX>``` - number of tx that were delivered on this QUIC
      connection that were already seen on a this or a different QUIC connection.
  
    Column 7: ```<NUM_VOTE_TX>``` - number of vote tx that were delivered on this QUIC
      connection
  
    Column 8: ```<NUM_USER_TX>``` - number of user (i.e. non-vote) tx that were
      delivered on this QUIC connection
  
    Column 9: ```<NUM_BADFEE_TX>``` - number of tx that were delivered on this
      QUIC connection that were unable to pay their tx fee due to insufficient
      fee payer funds
  
    Column 10: ```<NUM_FEE_TX>``` - number of tx that were delivered on this QUIC
      connection that were able to pay their fee
  
    Column 11: ```<MIN_FEE>``` - minimum fee paid by any tx delivered on this QUIC
      connection
      
    Column 12: ```<MAX_FEE>``` - maximum fee paid by any tx delivered on this QUIC
      connection
      
    Column 13: ```<TOTAL_FEES>``` - total fees paid by all tx delivered on this QUIC
      connection

```<TIMESTAMP> dups <REMOTE_ADDRESS:REMOTE_PORT> <REMOTE_ADDRESS>...```
  - After a QUIC connection is closed, if the remote peer submitted any
     tx that were already submitted by other QUIC connections, this logs
     the addresses of the peers that also submitted the same tx.
     ```<REMOTE_ADDRESS>```... is a space-separated list of those addresses.

```<TIMESTAMP> tx <REMOTE_ADDRESS:REMOTE_PORT> <SIGNATURE> <LEADER_STATUS>```
  - Only if ```--vote_tx``` was specified, these lines are logged.  They
     give the signature of transactions received on a QUIC connection by
     a given remote peer.  <LEADER_STATUS> indicates whether or not the
     validator was leader at the time the tx was received, or how soon it
     would have been leader.  This value is logged as one of:
     "not" - if the validator wouldn't be leader within 200 slots
     "kinda" - if the validator will be leader in between 21 and 200 slots
     "soon" - if the validator will be leader in between 3 and 20 slots
     "very" - if the validator will be leader in 2 slots or less
     "leader" - if the validator is currently leader

```<TIMESTAMP> fee <REMOTE_ADDRESS:REMOTE_PORT> <SIGNATURE> <CU_LIMIT> <CU_USED> <LAMPORTS>```
  - Only if ```--vote_tx``` was specified, these lines are logged.  They
     give the fee that the tx paid when it landed in a block on chain.  Only
     tx which landed on chain will have this line logged  <CU_LIMIT> is how many
     CU the tx requested for its CU limit; <CU_USED> is how many CU the tx actually
     used.
     
```<TIMESTAMP> leader_upcoming <SLOTS>```
  - Starting at 200 slots before a validator's upcoming leader slots, logs once
    per leader slot about the leader's slots being upcoming.  Only logs for
    200 ... 2 slots (doesn't log when the leader's slots are 1 slot away, for
    technical reasons)
    
```<TIMESTAMP> leader_begin```
  - Logged when the validator's leader slots have begun
    
```<TIMESTAMP> leader_end```
  - Logged when the validator's leader slots have ended



