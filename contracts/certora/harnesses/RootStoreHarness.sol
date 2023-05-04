import "../../contracts/RootStore.sol";

contract RootStoreHarness is RootStore {
    constructor(uint64 nRoots) RootStore(nRoots) {}

    // calling internal functions
    function addRoot(uint256 newRoot) public {
        _addRoot(newRoot);
    }

    function containsRoot(uint256 root) public view returns (bool) {
        return _containsRoot(root);
    }
    
    function checkContainsRoot(uint256 root) public view {
        _checkContainsRoot(root);
    }
    
    // getting internal variables
    function getRoot(uint256 index) public view returns (uint256) {
        return _roots[index];
    }

    function getRootsMap(uint256 root) public view returns (bool) {
        return _rootsMap[root];
    }
    
    function getWriteHead() public view returns(uint64) {
        return _writeHead;
    }
        
}