async function sign(payload) {
	console.log('signing', payload);
	payload = JSON.parse(payload);

	const allInjected = await polkadotExtensionDapp.web3Enable('my cool dapp');
	if (allInjected.length === 0) throw new Error('no extension installed');

	const { meta: { source } } = await polkadotExtensionDapp.web3Accounts().then((accounts) => accounts.find(({ address }) => address === payload.address));
	const injector = await polkadotExtensionDapp.web3FromSource(source);

	const signPayload = injector?.signer?.signPayload;
	if (!signPayload) throw new Error('signatures not supported');

	const { signature } = await signPayload(payload);
	console.log(signature);
	return signature;
}

async function address(name) {
	if (!name) throw new Error('no name provided');

	const allInjected = await polkadotExtensionDapp.web3Enable('my cool dapp');
	if (allInjected.length === 0) throw new Error('no extension installed');

	const { address } = await polkadotExtensionDapp.web3Accounts()
		.then((accounts) => accounts.find(({ meta: { name: accountName } }) => accountName === name));
	return address;
}

this.integration = { sign, address };
