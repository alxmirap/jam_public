Description:

this batch includes a second version of the TokenLedger service, 
simplified in the sense that it deals with only one token that we need not to mint.

## Service code

However, the refinement and accumulation logic changes significantly.
The state is now kept outside the service, and it simply keeps in state a hash summary of its history.

This summary is rough and does not intend to be cryptographically robust or useful.
The intention was to place something quickly that could relate previous state and list of transfers
to produce a new state. The correct way of doing this would be using a Merkle Tree,
but I deliberately pushed that for a later time.

This service also demonstrates a few other things, which v1 did not:
- passing data to refinement via an extrinsic
- exporting segments of data during refinement
- there is still not an example of another package importing these segments, but I have a plan for that later.

The transfers could be passed as part of the payload. That has been done, for example, in v1.
In this version, we wanted to demonstrate the use of extrinsics, and so we removed this data
from the payload and pass it inside an extrinsic instead.

We export these same extrinsics to the D3L to illustrate how we can export segments. 
Note that only Transfer payloads export segments, but Reset ones don't, and the work package
has to be aware of that.

Note: the jam service does NOT check the sender has enough balance to send the transaction. 
This is a consequence of having the state in the builder: since JAM does not know any of the account's raw balances,
it can only trust that the builder is honest and prevents payments with insufficient funds.

Note: the function refine::on_transfer_batch in uncharacteristically short, and leaves much work to be done in lib.rs.
This is in fact because when we extract the transfers from the extrinsic and try to verify them inside refinement.rs
we tend to get a Vm panic, that does not happen when the code is in lib.rs.
I have not been able to find the root cause: although the error seems to be in calling `key.verify`, it has nothing to do
with cryptography correctness: the values involved are all correct.

It is more likely the error arises from some memory management inside the VM, 
and in particular in the interaction of a loop with the signature verification.
But I don't think it is worth spending significant time debugging that for a tutorial.

## Client/Builder

This batch provides also an external entity, which I call builder because it produces work packages
and submits them to a node.
But it combines also the functionality of a small demo client, by having a console where we can
create new transfers and send them in a package at appropriate times.
This client also has a command to list the transfers submitted since the last reset.

Further notes:
- reset is a convenience for tests. Because we can have a long-running service used in different client 
sessions, and this service does not store raw state, our builder can easily get out of sync with the state
asserted on chain. Reset allows us to define the state known to the chain to match that of the builder.
- transfers sent to the chain are netted. That is, if we have account A send X tokens to B, and B send Y tokens to A,
the builder will find the net balance between A and B and create the corresponding transfer.

