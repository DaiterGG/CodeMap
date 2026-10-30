use libavoid::{
    ConnEnd, ConnRef, ConnType, Point, Polygon, PolygonInterface, Router, RoutingOption,
    RoutingParameter, ROUTER_FLAG_NONE,
};
use std::slice;

const SHAPE_BUFFER_DISTANCE: f64 = 12.0;
const IDEAL_NUDGING_DISTANCE: f64 = 8.0;
const PORT_OFFSET: f64 = 12.0;
const RIGHT: u32 = libavoid::CONN_DIR_RIGHT;
const UP: u32 = libavoid::CONN_DIR_UP;

#[derive(Clone, Copy)]
enum Port {
    Top,
    Bottom,
    Left,
    Right,
}

#[no_mangle]
pub extern "C" fn alloc_f64(length: usize) -> *mut f64 {
    let mut values = vec![0.0; length];
    let pointer = values.as_mut_ptr();
    std::mem::forget(values);
    pointer
}

#[no_mangle]
pub unsafe extern "C" fn free_f64(pointer: *mut f64, length: usize) {
    if !pointer.is_null() {
        drop(Vec::from_raw_parts(pointer, length, length));
    }
}

#[no_mangle]
pub unsafe extern "C" fn route_graph(
    node_data: *const f64,
    node_count: usize,
    edge_data: *const f64,
    edge_count: usize,
    output: *mut f64,
    output_capacity: usize,
) -> usize {
    if node_data.is_null() || edge_data.is_null() || output.is_null() {
        return 0;
    }

    let nodes = slice::from_raw_parts(node_data, node_count.saturating_mul(4));
    let edges = slice::from_raw_parts(edge_data, edge_count.saturating_mul(4));
    let Some(routes) = calculate_routes(nodes, node_count, edges, edge_count) else {
        return 0;
    };
    if routes.len() > output_capacity {
        return 0;
    }
    slice::from_raw_parts_mut(output, output_capacity)[..routes.len()].copy_from_slice(&routes);
    routes.len()
}

fn calculate_routes(
    nodes: &[f64],
    node_count: usize,
    edges: &[f64],
    edge_count: usize,
) -> Option<Vec<f64>> {
    if nodes.len() != node_count.checked_mul(4)?
        || edges.len() != edge_count.checked_mul(4)?
        || nodes.iter().chain(edges).any(|value| !value.is_finite())
    {
        return None;
    }

    let mut router = Router::new(ROUTER_FLAG_NONE);
    router.set_routing_parameter(RoutingParameter::ShapeBufferDistance, SHAPE_BUFFER_DISTANCE);
    router.set_routing_parameter(
        RoutingParameter::IdealNudgingDistance,
        IDEAL_NUDGING_DISTANCE,
    );
    router.set_routing_option(RoutingOption::NudgeOrthogonalRoutes, true);
    router.set_routing_option(RoutingOption::PenaliseOrthogonalSharedPathsAtConnEnds, true);
    for (index, bounds) in nodes.chunks_exact(4).enumerate() {
        let [x, y, width, height] = [bounds[0], bounds[1], bounds[2], bounds[3]];
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let mut polygon = Polygon::with_capacity(4);
        polygon.push(Point::new(x, y));
        polygon.push(Point::new(x + width, y));
        polygon.push(Point::new(x + width, y + height));
        polygon.push(Point::new(x, y + height));
        router.add_shape(polygon, u32::try_from(index + 1).ok()?);
    }

    for (index, endpoints) in edges.chunks_exact(4).enumerate() {
        let from_index = usize::try_from(index_from_float(endpoints[0])?).ok()?;
        let to_index = usize::try_from(index_from_float(endpoints[1])?).ok()?;
        if from_index >= node_count || to_index >= node_count {
            return None;
        }
        let from = &nodes[from_index * 4..from_index * 4 + 4];
        let to = &nodes[to_index * 4..to_index * 4 + 4];
        let from_center = Point::new(from[0] + from[2] / 2.0, from[1] + from[3] / 2.0);
        let to_center = Point::new(to[0] + to[2] / 2.0, to[1] + to[3] / 2.0);
        let source_port = port_from_float(endpoints[2])?;
        let destination_port = port_from_float(endpoints[3])?;
        let (source, source_direction, destination, destination_direction) =
            if from_index == to_index {
                (
                    Point::new(from[0] + from[2], from_center.y),
                    RIGHT,
                    Point::new(from_center.x, from[1]),
                    UP,
                )
            } else {
                (
                    port_position(from_center, source_port),
                    libavoid::CONN_DIR_ALL,
                    port_position(to_center, destination_port),
                    libavoid::CONN_DIR_ALL,
                )
            };
        let mut connector = ConnRef::with_endpoints(
            u32::try_from(index + 1).ok()?,
            ConnEnd::free_point(source, source_direction),
            ConnEnd::free_point(destination, destination_direction),
        );
        connector.set_routing_type(ConnType::Orthogonal);
        router.add_connector(connector);
    }

    router.process_transaction();

    let mut output = Vec::new();
    for index in 0..edge_count {
        let route = router
            .get_connector(u32::try_from(index + 1).ok()?)?
            .display_route()?;
        if route.size() < 2 {
            return None;
        }
        output.push(route.size() as f64);
        for point in route.points() {
            output.push(point.x);
            output.push(point.y);
        }
    }
    Some(output)
}

fn port_from_float(value: f64) -> Option<Port> {
    match index_from_float(value)? {
        1 => Some(Port::Top),
        2 => Some(Port::Bottom),
        3 => Some(Port::Left),
        4 => Some(Port::Right),
        _ => None,
    }
}

fn port_position(center: Point, port: Port) -> Point {
    match port {
        Port::Top => Point::new(center.x, center.y - PORT_OFFSET),
        Port::Bottom => Point::new(center.x, center.y + PORT_OFFSET),
        Port::Left => Point::new(center.x - PORT_OFFSET, center.y),
        Port::Right => Point::new(center.x + PORT_OFFSET, center.y),
    }
}

fn index_from_float(value: f64) -> Option<u32> {
    if value < 0.0 || value.fract() != 0.0 || value > u32::MAX as f64 {
        return None;
    }
    Some(value as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_around_intervening_node() {
        let nodes = [
            0.0, 0.0, 60.0, 60.0, 100.0, 0.0, 60.0, 80.0, 220.0, 0.0, 60.0, 60.0,
        ];
        let edges = [0.0, 2.0, 2.0, 1.0];
        let route = calculate_routes(&nodes, 3, &edges, 1).expect("route should be available");
        let point_count = route[0] as usize;
        assert!(point_count > 2);
        assert!(route[1..]
            .chunks_exact(2)
            .any(|point| point[1] < 0.0 || point[1] > 80.0));
    }

    #[test]
    fn routes_between_node_centers() {
        let nodes = [40.0, 40.0, 60.0, 60.0, 180.0, 40.0, 60.0, 60.0];
        let edges = [0.0, 1.0, 2.0, 1.0];
        let route = calculate_routes(&nodes, 2, &edges, 1).expect("route should be available");
        let point_count = route[0] as usize;
        assert_eq!(route[1], 70.0);
        assert_eq!(route[2], 82.0);
        assert_eq!(route[1 + (point_count - 1) * 2], 210.0);
        assert_eq!(route[2 + (point_count - 1) * 2], 58.0);
    }

    #[test]
    fn routes_vertical_edges_between_node_centers() {
        let nodes = [40.0, 40.0, 60.0, 60.0, 40.0, 180.0, 60.0, 60.0];
        let edges = [0.0, 1.0, 2.0, 1.0];
        let route = calculate_routes(&nodes, 2, &edges, 1).expect("route should be available");
        let point_count = route[0] as usize;
        assert_eq!(route[1], 70.0);
        assert_eq!(route[2], 82.0);
        assert_eq!(route[1 + (point_count - 1) * 2], 70.0);
        assert_eq!(route[2 + (point_count - 1) * 2], 198.0);
    }

    #[test]
    fn routes_bidirectional_edges_from_opposing_side_ports() {
        let nodes = [40.0, 40.0, 60.0, 60.0, 180.0, 40.0, 60.0, 60.0];
        let edges = [0.0, 1.0, 4.0, 3.0, 1.0, 0.0, 3.0, 4.0];
        let routes = calculate_routes(&nodes, 2, &edges, 2).expect("routes should be available");
        let first_point_count = routes[0] as usize;
        let second_route_offset = 1 + first_point_count * 2;
        let second_point_count = routes[second_route_offset] as usize;

        assert_eq!((routes[1], routes[2]), (82.0, 70.0));
        assert_eq!(
            (
                routes[1 + (first_point_count - 1) * 2],
                routes[2 + (first_point_count - 1) * 2]
            ),
            (198.0, 70.0)
        );
        assert_eq!(
            (
                routes[second_route_offset + 1],
                routes[second_route_offset + 2]
            ),
            (198.0, 70.0)
        );
        assert_eq!(
            (
                routes[second_route_offset + 1 + (second_point_count - 1) * 2],
                routes[second_route_offset + 2 + (second_point_count - 1) * 2]
            ),
            (82.0, 70.0)
        );
    }

    #[test]
    fn routes_self_edges_around_the_node() {
        let nodes = [40.0, 40.0, 60.0, 60.0];
        let edges = [0.0, 0.0, 2.0, 1.0];
        let route = calculate_routes(&nodes, 1, &edges, 1).expect("self route should be available");
        assert!(route[0] > 2.0);
    }
}
