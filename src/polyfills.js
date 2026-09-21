/**
 * Polyfills for Android 11 WebView (Chrome 80) compatibility
 * Must be imported as the FIRST module in the app entry point
 */

// Array.prototype.at - Chrome 92+
// 必须用 defineProperty 设置为不可枚举，否则 pdfjs-dist 会报错
if (!Array.prototype.at) {
  Object.defineProperty(Array.prototype, 'at', {
    value: function (n) {
      n = Math.trunc(n) || 0;
      if (n < 0) n += this.length;
      if (n < 0 || n >= this.length) return undefined;
      return this[n];
    },
    writable: true,
    configurable: true,
    enumerable: false,
  });
}

// String.prototype.at - Chrome 92+
if (!String.prototype.at) {
  Object.defineProperty(String.prototype, 'at', {
    value: function (n) {
      n = Math.trunc(n) || 0;
      if (n < 0) n += this.length;
      if (n < 0 || n >= this.length) return undefined;
      return this[n];
    },
    writable: true,
    configurable: true,
    enumerable: false,
  });
}

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
// 注意: JSON.parse(JSON.stringify()) 会丢失 Uint8Array 等 TypedArray 类型，
// 导致 pdfjs LoopbackPort 传递 PDF 数据时失败（stream.length = 0）
if (typeof structuredClone === 'undefined') {
  window.structuredClone = function deepClone(obj, transfers) {
    if (obj === null || obj === undefined) return obj;
    if (typeof obj !== 'object') return obj;
    // TypedArrays: 按字节复制，保留类型
    if (obj instanceof Uint8Array) {
      if (obj.constructor === obj.constructor) {
        const copy = new obj.constructor(obj.length);
        copy.set(obj);
        return copy;
      }
    }
    if (ArrayBuffer.isView(obj)) {
      const TypedArr = obj.constructor;
      const copy = new TypedArr(obj.length);
      copy.set(obj);
      return copy;
    }
    if (obj instanceof ArrayBuffer) {
      return obj.slice(0);
    }
    if (obj instanceof Date) {
      return new Date(obj.getTime());
    }
    if (obj instanceof RegExp) {
      return new RegExp(obj.source, obj.flags);
    }
    if (Array.isArray(obj)) {
      return obj.map(item => deepClone(item, transfers));
    }
    // Map
    if (obj instanceof Map) {
      const result = new Map();
      obj.forEach((v, k) => result.set(deepClone(k), deepClone(v)));
      return result;
    }
    // Set
    if (obj instanceof Set) {
      const result = new Set();
      obj.forEach(v => result.add(deepClone(v)));
      return result;
    }
    // Plain object
    const result = {};
    for (const key in obj) {
      if (Object.prototype.hasOwnProperty.call(obj, key)) {
        result[key] = deepClone(obj[key], transfers);
      }
    }
    return result;
  };
}

console.log('[Polyfills] Loaded for WebView compatibility');
