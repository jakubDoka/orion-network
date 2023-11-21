async function sign(data) {
	const allInjected = await polkadotExtensionDapp.web3Enable('my cool dapp');
	if (allInjected.length === 0) throw new Error('no extension installed');

	const [{ address, meta: { source } }] = await polkadotExtensionDapp.web3Accounts();
	const injector = await polkadotExtensionDapp.web3FromSource(source);

	const signRaw = injector?.signer?.signRaw;
	if (!signRaw) throw new Error('signatures not supported');

	const { signature } = await signRaw({ address, data, type: 'bytes' });
	return signature;
}

this.integration = { sign };
