# txingest receiver

This implements a simple txingest receiver.  In order to function, it requires that a Solana validator
be patched with the txingest patch; see:

https://github.com/bji/solana/tree/v1.17.30_txingest

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

```  txingest <LISTEN_ADDRESS> <PORT>```

Or three arguments:

```  txingest --show_tx <LISTEN_ADDRESS> <PORT>```

Typically this would be run on the validator itself, and would be run like so:

```  txingest 127.0.0.1 25555```

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

Every log line is of the form:

```<TIMESTAMP> <VERB> <PEER> <DETAILS>...```

```<TIMESTAMP>``` is always present and is the number of milliseconds since Unix epoch of the
event.

```<VERB>``` is always present and indicates what is being logged.

```<PEER>``` is always present and identifies the remote peer by IP address, and for established
QUIC connections, also indicates the remote port.

```<DETAILS>``` are always present, but vary by event logged.  See below.

# the log lines - specifics

The following lines are logged:

```<TIMESTAMP> failed <REMOTE_ADDRESS> <COUNT>```
 - Logged every 100 connection establishment protocol failures or every 1 second for a
    given remote address, whichever comes first.  ```<COUNT>``` is the number of failures for
    this remote address that occurred within the previous 10 seconds.

```<TIMESTAMP> exceeded <REMOTE_ADDRESS> <STAKE> <COUNT>```
 - Logged every 100 connection establishment failures due to remote peer exceeding
    connection limits, or every 1 second for a given remote address, whichever comes
    first.  ```<STAKE>``` is the lamports of stake of the remote peer.  NOTE that currently
    this is always 0; this field will likely be removed soon so don't count on it.
    ```<COUNT>``` is the number of failures due to exceeded connection limits for this
    remote address that occurred within the previous 10 seconds.

```<TIMESTAMP> started <REMOTE_ADDRESS:REMOTE_PORT> <STAKE>```
  - Logged on every new QUIC connection.  This appears to be logged multiple times
     in a row sometimes for a given remote address/port combo, and it's not clear
     why that happens.  ```<STAKE>``` is 0 for unstaked peers, or the lamports of stake
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

```<TIMESTAMP> tx <REMOTE_ADDRESS:REMOTE_PORT> <SIGNATURE>```
  - Only if ```--vote_tx``` was specified, these lines are logged.  They
     give the signature of transactions received on a QUIC connection by
     a given remote peer.

```<TIMESTAMP> leader_upcoming <SLOTS>```
  - Starting at 200 slots before a validator's upcoming leader slots, logs once
    per leader slot about the leader's slots being upcoming.  Only logs for
    200 ... 2 slots (doesn't log when the leader's slots are 1 slot away, for
    technical reasons)
    
```<TIMESTAMP> leader_begin```
  - Logged when the validator's leader slots have begun
    
```<TIMESTAMP> leader_end```
  - Logged when the validator's leader slots have ended



