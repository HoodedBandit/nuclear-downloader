export const performanceClockPath = '/?nuclear-performance-clock=isolated';

export function performanceClockIsolation() {
  return {
    name: 'nuclear-performance-clock-isolation',
    configureServer(server) {
      server.middlewares.use((request, response, next) => {
        const url = new URL(request.url ?? '/', 'http://renderer.invalid');
        if (`${url.pathname}${url.search}` === performanceClockPath) {
          response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
          response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
        }
        next();
      });
    }
  };
}
