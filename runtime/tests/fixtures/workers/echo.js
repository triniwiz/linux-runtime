onmessage = (e) => {
  const m = e.data;
  postMessage({ echo: m, isMap: m.map instanceof Map, year: m.date.getUTCFullYear(), len: strlen('from a worker') });
  if (m.last) close();
};
