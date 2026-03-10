Next work:

- implement a proper Merkle Tree instead of the current state root
- implement a 2-phase transfer service, to illustrate importing segments from the D3L. In this pattern:
    * builder creates two WorkPackages, instead of 1: a DebitWP and a CreditWP.
    * DebitWP debits all the sender accounts and transfers them to a clearing-house account.
    * DebitWP still receives the raw transfers in extrinsics, but no longer exports them as such to the D3L
    * Instead, DebitWP exports into the D3L the counterparts of the transfers it has made: from the clearing house to the intended receivers
    * CreditWP reads its data from the segment exported by DebitWP, and processes them completing the transfer
    * CreditWP must be created with DebitWP as a pre-requisite.
    * Packages still work as before, that is: they merely verify the raw transactions, and they enforce the state root updates.

- This is a somewhat artificial scenario, derived from a concept where the balances were still fully held by the Jam chain.
In this scenario, transfers were sharded so that the same core always handled the operations on a given account, 
but since we can't shard simultaneously on the sender and the received, we would break the transfer in two phases to first shard 
on the sender, and then shard on the receiver.

- With full state being maintained outside the chain, it is harder to find a justification for this use-case, but it serves as illustration
of several techniques: pre-requisite packages, exporting and reading segments.