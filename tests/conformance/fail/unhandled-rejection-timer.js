// expect: Uncaught (in promise) Error: y
setTimeout(() => Promise.reject(new Error('y')), 1);
