initSidebarItems({"enum":[["VerifySigError",""]],"fn":[["is_pk_key","Check if the given storage key is a public key. If it is, returns the owner."],["pk_key","Obtain a storage key for user’s public key."],["sign","Sign the data with a key."],["verify_signature","Check that the public key matches the signature on the given data."],["verify_signature_raw","Check that the public key matches the signature on the given raw data."]],"struct":[["Keypair","An ed25519 keypair."],["PublicKey",""],["PublicKeyHash",""],["SecretKey","An EdDSA secret key."],["Signature",""],["Signed","A generic signed data wrapper for Borsh encode-able data."],["SignedTxData","This can be used to sign an arbitrary tx. The signature is produced and verified on the tx data concatenated with the tx code, however the tx code itself is not part of this structure."]],"type":[["SignatureError","Errors which may occur while processing signatures and keypairs."]]});