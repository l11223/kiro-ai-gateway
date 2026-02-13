import { Link, useLocation } from 'react-router-dom';
import { NavigationDropdown } from './NavDropdowns';
import { isActive, getCurrentNavItem, type NavItem } from './constants';
import { useConfigStore } from '../../stores/useConfigStore';

interface NavMenuProps {
    navItems: NavItem[];
}

export function NavMenu({ navItems }: NavMenuProps) {
    const location = useLocation();
    const { isMenuItemHidden } = useConfigStore();
    const visibleNavItems = navItems.filter(item => !isMenuItemHidden(item.path));

    const activeClass = 'bg-gray-900 text-white shadow-sm dark:bg-white dark:text-gray-900';
    const inactiveClass = 'text-gray-700 hover:text-gray-900 hover:bg-gray-200 dark:text-gray-400 dark:hover:text-base-content dark:hover:bg-base-100';

    return (
        <>
            {/* Text pills (≥ 1120px) */}
            <nav className="max-[1119px]:hidden flex items-center gap-1 bg-gray-100 dark:bg-base-200 rounded-full p-1">
                {visibleNavItems.map(item => (
                    <Link key={item.path} to={item.path} draggable="false"
                        className={`px-4 xl:px-6 py-2 rounded-full text-sm font-medium transition-all whitespace-nowrap ${isActive(location.pathname, item.path) ? activeClass : inactiveClass}`}>
                        {item.label}
                    </Link>
                ))}
            </nav>

            {/* Icon pills (640px - 1120px) */}
            <nav className="max-[639px]:hidden min-[1120px]:hidden flex items-center gap-1 bg-gray-100 dark:bg-base-200 rounded-full p-1">
                {visibleNavItems.map(item => (
                    <Link key={item.path} to={item.path} draggable="false"
                        className={`p-2 rounded-full transition-all ${isActive(location.pathname, item.path) ? activeClass : inactiveClass}`}
                        title={item.label}>
                        <item.icon className="w-5 h-5" />
                    </Link>
                ))}
            </nav>

            {/* Icon+text dropdown (375px - 640px) */}
            <div className="max-[374px]:hidden min-[640px]:hidden block">
                <NavigationDropdown navItems={visibleNavItems}
                    isActive={(path) => isActive(location.pathname, path)}
                    getCurrentNavItem={() => getCurrentNavItem(location.pathname, visibleNavItems)}
                    onNavigate={() => {}} showLabel={true} />
            </div>

            {/* Icon dropdown (< 375px) */}
            <div className="min-[375px]:hidden">
                <NavigationDropdown navItems={visibleNavItems}
                    isActive={(path) => isActive(location.pathname, path)}
                    getCurrentNavItem={() => getCurrentNavItem(location.pathname, visibleNavItems)}
                    onNavigate={() => {}} showLabel={false} />
            </div>
        </>
    );
}
