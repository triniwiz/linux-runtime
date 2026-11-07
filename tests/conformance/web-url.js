const { eq } = require('./harness');

const u = new URL('../b/c?x=1&y=two words#frag', 'https://user:pw@example.com:8080/a/index.html');
eq([u.href, u.origin, u.host, u.pathname, u.search, u.hash, u.username],
   ['https://user:pw@example.com:8080/b/c?x=1&y=two%20words#frag', 'https://example.com:8080', 'example.com:8080', '/b/c', '?x=1&y=two%20words', '#frag', 'user'], 'parse');
eq(u.searchParams.get('y'), 'two words', 'searchParams');
u.searchParams.append('z', 'a&b');
eq(u.search, '?x=1&y=two+words&z=a%26b', 'params update url');
u.pathname = '/new path';
u.port = '';
eq(u.href, 'https://user:pw@example.com/new%20path?x=1&y=two+words&z=a%26b#frag', 'setters');
eq(URL.canParse('nope'), false, 'canParse');
let threw = false; try { new URL('not a url'); } catch (e) { threw = e instanceof TypeError; }
eq(threw, true, 'invalid');
eq(new URLSearchParams({ a: 1, b: 'x y' }).toString(), 'a=1&b=x+y', 'URLSearchParams');
eq(new URL('file:///tmp/a b.txt').pathname, '/tmp/a%20b.txt', 'file url');
