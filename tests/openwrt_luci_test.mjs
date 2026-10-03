// 模拟 LuCI 的 RPC / 表单接口，验证状态、固定服务动作及控制台链接。
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const source = readFileSync(new URL('../openwrt/package/luci-app-campus-auth/htdocs/luci-static/resources/view/campus-auth.js', import.meta.url), 'utf8');
const acl = JSON.parse(readFileSync(new URL('../openwrt/package/luci-app-campus-auth/root/usr/share/rpcd/acl.d/luci-app-campus-auth.json', import.meta.url), 'utf8'))['luci-app-campus-auth'];
const consoleURL = new Function(source.slice(0, source.indexOf('var callService')) +
	source.slice(source.indexOf('function consoleURL'), source.indexOf('return view.extend')) + 'return consoleURL;')();
assert.equal(consoleURL('127.0.0.1', '50721', 'router'), null);
assert.equal(consoleURL('::1', '50721', 'router'), null);
assert.equal(consoleURL('192.168.1.1', '50721', 'router'), 'http://192.168.1.1:50721/');
assert.equal(consoleURL('::', '50721', '2001:db8::1'), 'http://[2001:db8::1]:50721/');
assert.equal(consoleURL('0.0.0.0', '50721', '[2001:db8::1]'), 'http://[2001:db8::1]:50721/');
assert.equal(consoleURL('javascript:alert(1)', '50721', 'router'), null);
assert.equal(consoleURL('192.168.1.1', '0', 'router'), null);
assert.equal(consoleURL('192.168.1.1', '65536', 'router'), null);

let services = { 'campus-auth': { instances: { first: { running: false }, second: { running: true } } } };
let execCode = 0;
let allowed = true;
const commands = [], notifications = [], options = [], polls = [];
const E = (tag, attrs, children) => ({ tag, attrs, children });
const form = {
	Map: class {
		section() { return { option: (type, name) => { const option = { name }; options.push(option); return option; } }; }
		render() { return Promise.resolve(E('form', {}, [])); }
	}
};
const instance = new Function('view', 'form', 'uci', 'rpc', 'fs', 'ui', 'poll', 'L', 'E', '_', 'window', source)(
	{ extend: value => value }, form,
	{ load: async () => {}, get: (config, section, name) => ({ host: '192.168.1.1', port: '50721' })[name] },
	{ declare: () => async name => { assert.equal(name, 'campus-auth'); return services; } },
	{ exec: async (path, args) => {
		// HTTP RPC 与命令范围分别校验，仅有命令 ACL 不能通过 RPC 入口。
		assert.ok(acl.read.ubus.file.includes('exec'));
		const files = { ...acl.read.file, ...(allowed ? acl.write.file : {}) };
		assert.ok(files[[path, ...args].join(' ')]?.includes('exec'));
		commands.push([path, args]);
		return { code: execCode, stderr: '失败原因' };
	} },
	{ addNotification: (...args) => notifications.push(args) },
	{ add: (...args) => polls.push(args) },
	{ bind: (fn, context, ...args) => fn.bind(context, ...args), hasViewPermission: () => allowed },
	E, text => text, { location: { hostname: 'router' } }
);
assert.deepEqual(await instance.readStatus(), { running: true, autostart: true });
execCode = 1;
services = {};
assert.deepEqual(await instance.readStatus(), { running: false, autostart: false });
execCode = 2;
await assert.rejects(instance.readStatus(), /失败原因/);
execCode = 0;
allowed = false;
const tree = await instance.render(await instance.load());
assert.equal(polls.length, 1);
assert.equal(polls[0][1], 5);
const buttons = tree.children.find(node => node.tag === 'div').children;
assert.equal(buttons.length, 5);
assert.ok(buttons.every(button => button.attrs.disabled));
assert.equal(tree.children.find(node => node.tag === 'a').attrs.href, 'http://192.168.1.1:50721/');
assert.equal(options.find(option => option.name === 'port').validate('main', '0'), '端口必须在 1 至 65535 之间。');
assert.equal(options.find(option => option.name === 'data_dir').validate('main', '/'), '请输入非根目录的绝对路径。');
assert.equal(options.find(option => option.name === 'data_dir').validate('main', '/etc/campus-auth'), true);
allowed = true;
for (const action of ['start', 'stop', 'restart', 'enable', 'disable']) {
	const button = { disabled: false };
	await instance.runAction(action, { currentTarget: button });
	assert.ok(commands.some(([path, args]) => path === '/etc/init.d/campus-auth' && args.length === 1 && args[0] === action));
	assert.equal(button.disabled, false);
}
execCode = 1;
await instance.runAction('restart', { currentTarget: { disabled: false } });
assert.equal(notifications.length, 1);
assert.equal(notifications[0][1].children, '失败原因');
console.log('LuCI 检查通过：控制台链接、RPC 状态、权限、表单校验、五种服务动作及失败反馈。');
