/**
 * Polyfills for Android WebView 110 (Chrome 110) compatibility
 * Must be imported as the FIRST module in the app entry point
 */

// Promise.withResolvers - Chrome 124+
if (!Promise.withResolvers) {
  Promise.withResolvers = function () {
    let resolve, reject;
    const promise = new Promise((res, rej) => {
      resolve = res;
      reject = rej;
    });
    return { promise, resolve, reject };
  };
}

// Promise.try - Chrome 128+
if (!Promise.try) {
  Promise.try = function (callback) {
    return new Promise((resolve, reject) => {
      try {
        resolve(callback());
      } catch (e) {
        reject(e);
      }
    });
  };
}

// URL.parse - Chrome 123+
if (!URL.parse) {
  URL.parse = function (url, base) {
    try {
      return new URL(url, base);
    } catch {
      return null;
    }
  };
}

// Array.fromAsync - Chrome 123+
if (!Array.fromAsync) {
  Array.fromAsync = async function (arrayLike, mapFn, thisArg) {
    const arr = [];
    const length = arrayLike.length ?? 0;
    for (let i = 0; i < length; i++) {
      const value = await (mapFn ? mapFn.call(thisArg, arrayLike[i], i) : arrayLike[i]);
      arr.push(value);
    }
    return arr;
  };
}

// Object.hasOwn - Chrome 93+ (should be fine but just in case)
if (!Object.hasOwn) {
  Object.hasOwn = function (obj, prop) {
    return Object.prototype.hasOwnProperty.call(obj, prop);
  };
}

// structuredClone - Chrome 98+ (should be fine but just in case)
if (typeof structuredClone === 'undefined') {
  window.structuredClone = function (obj) {
    return JSON.parse(JSON.stringify(obj));
  };
}

console.log('[Polyfills] Loaded for WebView compatibility');
