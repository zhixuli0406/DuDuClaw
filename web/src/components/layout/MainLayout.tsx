import { Outlet, useLocation } from 'react-router';
import { Sidebar } from './Sidebar';
import { Header } from './Header';

export function MainLayout() {
  const location = useLocation();
  return (
    <div className="flex h-screen overflow-hidden">
      {/* Fixed ambient stage the glass surfaces refract */}
      <div className="app-ambient" aria-hidden="true" />
      <Sidebar />
      <div className="flex flex-1 flex-col overflow-hidden">
        <Header />
        <main className="flex-1 overflow-y-auto p-6">
          {/* Re-key on route change to replay the entrance reveal */}
          <div key={location.pathname} className="page-enter">
            <Outlet />
          </div>
        </main>
      </div>
    </div>
  );
}
