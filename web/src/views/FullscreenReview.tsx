import type { Route } from "@puppet-master/client-core/router";
import { sessionRoutePath } from "@puppet-master/client-core/router";
import { ErrorBoundary } from "../components/ErrorBoundary";
import { navigate } from "../router";
import { ReviewPage } from "./ReviewPage";

export function FullscreenReview({ route, ownsWindow }: {
  route: Extract<Route, { name: "review" }>;
  ownsWindow: boolean;
}) {
  return (
    <div className="shell shell-fullbleed is-focus-mode review-window">
      <main className="main-pane">
        <ErrorBoundary resetKey={route.id}>
          <ReviewPage
            key={route.id}
            id={route.id}
            at={{ view: route.view, file: route.file, thread: route.thread }}
            onExit={(sessionId) => navigate(sessionRoutePath(sessionId, "agent"))}
            onFinished={() => {
              if (!ownsWindow) return false;
              window.close();
              return true;
            }}
          />
        </ErrorBoundary>
      </main>
    </div>
  );
}
